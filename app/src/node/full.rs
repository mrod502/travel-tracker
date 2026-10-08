//! FullNode implementation for Phase 0.
//!
//! This module provides the complete Phase 0 node implementation that:
//! 1. Monitors Bluetooth devices
//! 2. Signs occurrences with node identity
//! 3. Rate-limits storage to avoid duplicates
//! 4. Stores signed occurrences in the database
//!
//! # Revocation checking
//!
//! With `[revocation].enabled = true` the node loads its CA's list at startup,
//! refreshes it in the background, and asks it before recording an occurrence —
//! [`crate::node::revocation::RevocationWatch`] for what a list can and cannot say,
//! and `FullNode::authorize_store` for the gate. Enabled but unusable — no anchor,
//! no published list, a list that does not verify under the anchor, one whose window
//! has closed, one older than `[revocation].max_staleness_secs` — is a startup error
//! rather than a degraded node.
//!
//! With it off, occurrences are stored without asking anyone, which is what the
//! setting means. It is off by default because it requires a CA that publishes
//! lists, and a node configured to consult one that does not exist is a node that
//! does not run.
//!
//! [`FullNode::verify_received_occurrence`] and
//! [`FullNode::should_allow_peer_connection`] exist and still have no caller —
//! there is no transport to receive either an occurrence or a handshake. They are
//! written against the watch rather than against a checker that holds nothing, so
//! the day a transport lands the revocation path is already there.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐
//! │ Bluetooth Scan  │ → DeviceEvent → store_occurrence()
//! └─────────────────┘                  │
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ Rate Limiter     │ → Skip if limited
//!                             └──────────────────┘
//!                                      │ (pass)
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ Build Payload    │
//!                             └──────────────────┘
//!                                      │
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ CBOR Encode      │
//!                             └──────────────────┘
//!                                      │
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ Sign with Key    │
//!                             └──────────────────┘
//!                                      │
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ Store to DB      │
//!                             └──────────────────┘
//! ```
//!
//! # Example
//!
//! ```ignore
//! use app::node::full::FullNode;
//! use std::path::PathBuf;
//!
//! let data_dir = PathBuf::from("/var/lib/btmon");
//! let mut node = FullNode::new(data_dir).await?;
//! node.run().await?;
//! ```

use bt_mon::{BluetoothDevice, DeviceEvent, DeviceId, DeviceMonitor};
use chrono::{DateTime, Utc};
use futures_util::stream::StreamExt;
use log::{debug, error, info, warn};
use repo::models::LocationSource;
use repo::{NodeRepository, Occurrence, OccurrenceRepository, Pool, SignalType};
use sha2::Digest;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;
use uuid::Uuid;

use crate::config::RevocationConfig;
use crate::error::{AppError, Result};
use crate::node::identity::NodeIdentity;
use crate::node::revocation::RevocationWatch;
use crate::node::{
    derive_device_identity, AdvertisementContent, Clock, Decision, DeviceIdentity, Node,
    ObservationPolicy, ObservationStats, RateLimiterStats, SystemClock,
};
use crate::position::{BestEffortPositionSource, NoPositionSource, Position, PositionSource};
use crate::provenance::encode::encode_payload;
use crate::provenance::payload::{truncate_to_micros, PayloadV2, CURRENT_VERSION};

/// CoreBluetooth (and any other backend that hides the MAC address) yields
/// host-local identifiers, which changes what `device_hash` means. Worth saying
/// out loud exactly once rather than on every scan event.
static NON_MAC_IDENTIFIER_WARNED: Once = Once::new();

/// `store_raw_payload` promises the radio's bytes; a backend that cannot supply
/// them has to say so out loud, once, instead of storing a payload that quietly
/// lacks the key the operator turned on.
static RAW_PAYLOAD_ABSENT_WARNED: Once = Once::new();

/// How many sampling windows a device is trusted to stay present for, when
/// nothing was configured.
///
/// The trust has to be measured in windows because re-observation is what it
/// exists for: a device is only sampled again while present, and the sample is
/// due after one window. Anything under that and a node never re-observes at all.
/// Four lets a backend miss three reports in a row — a scan that pauses, an
/// adapter that resets, a beacon whose interval does not line up with ours —
/// before the node concludes the device has gone, and it keeps the lie short:
/// a device that left range without anyone noticing is sampled for at most four
/// windows more, from a reading no older than the last report.
const PRESENCE_TIMEOUT_WINDOWS: u32 = 4;

/// Floor on the re-observation tick, so a node sampling on a short window does
/// not spend its main loop asking itself who is due.
const MIN_SAMPLING_TICK: Duration = Duration::from_millis(250);

/// Ceiling on it, so a node sampling on a long window still re-observes near the
/// window boundary rather than a quarter of a window after it.
const MAX_SAMPLING_TICK: Duration = Duration::from_secs(5);

/// Configuration for FullNode.
#[derive(Clone)]
pub struct FullNodeConfig {
    /// Database connection pool.
    pub pool: Pool,

    /// Data directory for node identity and other persistent state.
    pub data_dir: PathBuf,

    /// Rate limiter threshold (default: 15 seconds).
    pub rate_limit_threshold_ms: u64,

    /// Optional max cache size for rate limiter.
    pub rate_limit_max_cache_size: Option<usize>,

    /// Where the node acquires the position it asserts in each occurrence.
    ///
    /// Defaults to [`NoPositionSource`], so a node with nothing configured stores
    /// a NULL location rather than a guessed one.
    pub position: Arc<dyn PositionSource>,

    /// Delay before reopening the device event stream after it closes
    /// (default: 1000 ms). Prevents a tight reopen loop when a backend's
    /// event stream closes immediately or flaps.
    pub stream_reopen_delay_ms: u64,

    /// Store the radio's own advertisement bytes in `signal_payload`.
    ///
    /// Gates the `ble.raw_payload_hex` key, which the column's own comment names
    /// as the payload's contents. Everything the node can parse out of an
    /// advertisement is already a column or another `signal_payload` key; the raw
    /// bytes are what is left over — the only way to re-read a capture after the
    /// parser learns a new structure. They are covered by the signature, so
    /// turning this on changes what the node attests to.
    ///
    /// A backend that does not hand over advertisement bytes (see
    /// `BluetoothDevice::raw_payload`) stores nothing extra. It then says so,
    /// once, when the first advertisement arrives without them, rather than
    /// pretending to be in effect.
    pub store_raw_payload: bool,

    /// The node identity this configuration asserts, if the operator stated one.
    ///
    /// Checked against the identity loaded from `data_dir` at startup, never used
    /// as an identity itself: a node id is SHA-256 of the signing key, so it can
    /// only ever be derived, not declared. `None` accepts whatever the key file
    /// says.
    pub expected_node_id: Option<Vec<u8>>,

    /// How long a device is trusted to still be in range after the last report
    /// for it, and so how long the node keeps re-observing it.
    ///
    /// `None` derives it as [`PRESENCE_TIMEOUT_WINDOWS`] sampling windows, which is
    /// what the value has to be measured in: re-observation is what this exists
    /// for, and a sample is only due after a window. Long enough and a device whose
    /// backend reports sparsely is not declared absent between two reports; short
    /// enough and one that left range without saying so stops being sampled.
    ///
    /// Deliberately not an operator setting. Its sensible value is a multiple of
    /// the window the operator already chose, and a knob here is a second number
    /// that can contradict the first — which is what the check in
    /// [`FullNode::new`] would then exist to catch.
    ///
    /// A value below the window makes presence expire before a sample is ever due,
    /// which turns re-observation off; startup rejects it.
    pub(crate) presence_timeout_ms: Option<u64>,

    /// Where the node reads time. `None` is the system clock.
    ///
    /// Not operator-configurable — it exists so a test can move time instead of
    /// sleeping through a window, and so the node's timestamps and its sampling
    /// intervals cannot come from two different places.
    pub(crate) clock: Option<Arc<dyn Clock>>,

    /// Use mock backend for testing/development (no physical Bluetooth required).
    #[cfg(feature = "mock")]
    pub use_mock_backend: bool,

    /// Revocation checking, from `[revocation]`.
    ///
    /// Default-disabled, and when enabled it is a hard requirement:
    /// [`FullNode::new`] fails if the node cannot load a current, verified list
    /// from the configured CA, because a node that stores occurrences it cannot
    /// attest to is worse than one that is visibly not running. See
    /// [`crate::node::revocation::RevocationWatch`].
    pub revocation: RevocationConfig,
}

impl std::fmt::Debug for FullNodeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FullNodeConfig")
            .field("pool", &"<Pool>")
            .field("data_dir", &self.data_dir)
            .field("rate_limit_threshold_ms", &self.rate_limit_threshold_ms)
            .field("rate_limit_max_cache_size", &self.rate_limit_max_cache_size)
            .field("position", &"<PositionSource>")
            .field("stream_reopen_delay_ms", &self.stream_reopen_delay_ms)
            .field("store_raw_payload", &self.store_raw_payload)
            .field(
                "expected_node_id",
                &self.expected_node_id.as_deref().map(hex::encode),
            )
            .field("revocation", &self.revocation)
            .finish()
    }
}

impl FullNodeConfig {
    /// Create a new configuration with default values.
    pub fn new(pool: Pool, data_dir: PathBuf) -> Self {
        Self {
            pool,
            data_dir,
            rate_limit_threshold_ms: 15_000, // 15 seconds default
            rate_limit_max_cache_size: None,
            position: Arc::new(NoPositionSource),
            stream_reopen_delay_ms: 1_000, // 1 second default
            store_raw_payload: true,
            expected_node_id: None,
            presence_timeout_ms: None,
            clock: None,
            #[cfg(feature = "mock")]
            use_mock_backend: false,
            revocation: RevocationConfig::default(),
        }
    }

    /// Set the rate limit threshold.
    pub fn with_rate_limit_threshold(mut self, threshold_ms: u64) -> Self {
        self.rate_limit_threshold_ms = threshold_ms;
        self
    }

    /// Set the max cache size for the rate limiter.
    pub fn with_rate_limit_max_cache_size(mut self, size: usize) -> Self {
        self.rate_limit_max_cache_size = Some(size);
        self
    }

    /// Set the source the node takes its position from.
    pub fn with_position(mut self, position: Arc<dyn PositionSource>) -> Self {
        self.position = position;
        self
    }

    /// Set the delay (in milliseconds) before reopening the device event
    /// stream after it closes.
    pub fn with_stream_reopen_delay(mut self, delay_ms: u64) -> Self {
        self.stream_reopen_delay_ms = delay_ms;
        self
    }

    /// Store (or drop) the radio's advertisement bytes in `signal_payload`.
    pub fn with_store_raw_payload(mut self, enabled: bool) -> Self {
        self.store_raw_payload = enabled;
        self
    }

    /// Trust a device present for `ms` after the last report for it.
    ///
    /// See [`FullNodeConfig::presence_timeout_ms`].
    pub(crate) fn with_presence_timeout(mut self, ms: u64) -> Self {
        self.presence_timeout_ms = Some(ms);
        self
    }

    /// Read time from `clock` rather than from the system.
    pub(crate) fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Assert the identity the key file has to agree with, as 32 raw bytes.
    ///
    /// A stated id that does not match the loaded key stops startup: see
    /// [`assert_expected_identity`].
    ///
    /// [`assert_expected_identity`]: crate::node::full::assert_expected_identity
    pub fn with_expected_node_id(mut self, node_id: Option<Vec<u8>>) -> Self {
        self.expected_node_id = node_id;
        self
    }
}

/// Refuse to start a node whose stated identity is not the key it loaded.
///
/// A node id is SHA-256 of the signing public key, so configuration can only
/// *assert* an identity, never supply one: the key file has the final say. The
/// assertion is still worth enforcing — two machines enrolled separately look
/// identical until something compares them, and a node that signs with key A
/// while its operator believes it is node B stores correctly signed occurrences
/// under the wrong name for as long as it runs.
///
/// `expected` is the configured id as 32 raw bytes (see
/// [`crate::config::parse_node_id`]); `None` means nothing was stated and the
/// key file stands alone.
///
/// # Errors
///
/// [`AppError::Config`] naming both ids and the key file, with the two ways out:
/// move the data directory to the promised key, or restate the id this node
/// actually has.
pub fn assert_expected_identity(
    actual: &[u8],
    expected: Option<&[u8]>,
    data_dir: &Path,
) -> Result<()> {
    let key_file = data_dir.join(NodeIdentity::IDENTITY_FILENAME);

    let Some(expected) = expected else {
        debug!(
            "No node id configured, using the key in {}",
            key_file.display()
        );
        return Ok(());
    };

    if actual == expected {
        info!("Node ID matches the configured node id");
        return Ok(());
    }

    let expected_hex = hex::encode(expected);
    let actual_hex = hex::encode(actual);

    Err(AppError::Config(format!(
        "the configured node id {expected_hex} is not this node's identity: the key in {} is \
         {actual_hex}.\nA node id is SHA-256 of the node's signing key and cannot be chosen here, \
         so either point BT_DATA_DIR / [bluetooth].data_dir at the directory holding the \
         {expected_hex} key, or correct --node-id / NODE_ID / [node].id to {actual_hex}.",
        key_file.display(),
    )))
}

/// Statistics about FullNode operation.
#[derive(Debug, Clone)]
pub struct FullNodeStats {
    /// Total number of device events received.
    pub total_events: usize,

    /// Number of occurrences stored.
    pub occurrences_stored: usize,

    /// Number of reports the sampling policy declined for being inside a device's
    /// window.
    pub occurrences_rate_limited: usize,

    /// Number of storage errors.
    pub storage_errors: usize,

    /// Number of times the device event stream was (re)opened after closing.
    pub stream_reopens: usize,

    /// What the sampling policy has decided, including how many rows came from
    /// the re-observation timer rather than from a report.
    pub sampling: ObservationStats,

    /// The window's own counters.
    pub rate_limiter_stats: RateLimiterStats,
}

/// The Phase 0 FullNode implementation.
///
/// A FullNode is a standalone node that:
/// - Monitors Bluetooth devices on its local adapter
/// - Signs all occurrences with its node identity
/// - Rate-limits storage to avoid duplicates
/// - Stores signed occurrences in its local database
///
/// It does *not* check revocation status before storing. The node holds a checker,
/// but no list is ever loaded into it — it answers `Unknown` for every node id —
/// and no code path consults it on the way to storage. See the module
/// documentation: that gap is GAP_ANALYSIS B11.
///
/// # Thread Safety
///
/// FullNode is `Send + Sync` and can be used from multiple threads.
/// The internal state (rate limiter, counters) is protected by atomics and DashMap.
pub struct FullNode {
    /// Node identity (key pair for signing).
    identity: NodeIdentity,

    /// Database connection pool.
    pool: Pool,

    /// When this node records an occurrence for a device it can see.
    ///
    /// Holds the rate limiter, and so the window: it is the only thing in the node
    /// that decides whether a report is worth a row, and a second limiter with its
    /// own idea of when a window closed is how a device ends up with two rows or
    /// none. See the [observation module](crate::node::observation) for why both
    /// bounds are needed and why a re-observation is a fresh reading, not a
    /// rewrite.
    sampling: Arc<ObservationPolicy>,

    /// Clock for timestamps.
    clock: Arc<dyn Clock>,

    /// Where the node is, acquired per occurrence.
    position: BestEffortPositionSource,

    /// The CA's revocation list, kept current, when `[revocation].enabled`.
    ///
    /// `None` means the operator turned checking off, which every gate reads as "no
    /// gate", not as a refusal. When it is on, the two gates that consult it answer
    /// differently for a node the list does not name — see
    /// [`Self::authorize_store`] and [`Self::should_allow_peer_connection`].
    revocation: Option<Arc<RevocationWatch>>,

    /// Delay before reopening a closed device event stream.
    stream_reopen_delay_ms: u64,

    /// Whether the radio's advertisement bytes belong in `signal_payload`.
    store_raw_payload: bool,

    /// Statistics counters.
    total_events: std::sync::atomic::AtomicUsize,
    occurrences_stored: std::sync::atomic::AtomicUsize,
    occurrences_rate_limited: std::sync::atomic::AtomicUsize,
    storage_errors: std::sync::atomic::AtomicUsize,
    stream_reopens: std::sync::atomic::AtomicUsize,
}

impl FullNode {
    /// Create a new FullNode instance.
    ///
    /// This loads or creates the node identity, initializes the rate limiter,
    /// and sets up the database connection.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for the node
    ///
    /// # Returns
    ///
    /// * `Ok(FullNode)` - A fully initialized node
    /// * `Err(AppError)` - If initialization failed
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::full::{FullNode, FullNodeConfig};
    /// use repo::Pool;
    /// use std::path::PathBuf;
    ///
    /// let pool = Pool::connect("postgres://localhost/test").await?;
    /// let config = FullNodeConfig::new(pool, PathBuf::from("/var/lib/btmon"))
    ///     .with_rate_limit_threshold_ms(15_000);
    ///
    /// let node = FullNode::new(config).await?;
    /// ```
    pub async fn new(config: FullNodeConfig) -> Result<Self> {
        // Load or create node identity
        let identity = NodeIdentity::load_or_create(&config.data_dir)
            .map_err(|e| AppError::Io(format!("Failed to load node identity: {}", e)))?;

        info!(
            "Node identity loaded/created. Node ID: {}",
            hex::encode(identity.node_id())
        );

        // A node id stated in configuration is an assertion about this key file,
        // not an identity the node can adopt, so it is checked rather than used.
        // Doing it here — before the first occurrence is signed — is what keeps a
        // mismatch from being discovered as a thousand rejected rows later.
        assert_expected_identity(
            identity.node_id(),
            config.expected_node_id.as_deref(),
            &config.data_dir,
        )?;

        // The clock every interval in this node is measured on, and every
        // timestamp in every row is read from. One object, so the two can never
        // disagree about what "now" means.
        let clock: Arc<dyn Clock> = config
            .clock
            .clone()
            .unwrap_or_else(|| Arc::new(SystemClock));

        // The sampling window, and how long a device is trusted to stay present
        // inside it. Both go to the policy, which owns the window from here on.
        let window = Duration::from_millis(config.rate_limit_threshold_ms);
        let presence_timeout = Duration::from_millis(
            config
                .presence_timeout_ms
                .unwrap_or(config.rate_limit_threshold_ms * u64::from(PRESENCE_TIMEOUT_WINDOWS)),
        );
        if presence_timeout < window {
            return Err(AppError::Config(format!(
                "presence timeout {}ms is shorter than the sampling window {}ms: a device's \
                 presence would expire before its first re-observation is due, so the node would \
                 record each device once and never sample it again. Set presence_timeout_ms to at \
                 least {}, or leave it unset.",
                presence_timeout.as_millis(),
                window.as_millis(),
                config.rate_limit_threshold_ms,
            )));
        }

        let sampling = Arc::new(ObservationPolicy::new(
            window,
            presence_timeout,
            clock.clone(),
        ));

        // The CA's revocation list, loaded before the node does anything with it.
        // Enabled and unusable is a startup error rather than a degraded node: see
        // RevocationWatch::start for what it refuses and why.
        let revocation = if config.revocation.enabled {
            let watch = RevocationWatch::start(&config.revocation, &config.pool).await?;
            info!("Revocation checking enabled — {}", watch.describe());
            Some(Arc::new(watch))
        } else {
            info!(
                "Revocation checking disabled: occurrences are stored without checking the \
                 reporting node against a revocation list"
            );
            None
        };

        Ok(Self {
            identity,
            pool: config.pool,
            sampling,
            clock,
            position: BestEffortPositionSource::new(config.position),
            revocation,
            stream_reopen_delay_ms: config.stream_reopen_delay_ms,
            store_raw_payload: config.store_raw_payload,
            total_events: std::sync::atomic::AtomicUsize::new(0),
            occurrences_stored: std::sync::atomic::AtomicUsize::new(0),
            occurrences_rate_limited: std::sync::atomic::AtomicUsize::new(0),
            storage_errors: std::sync::atomic::AtomicUsize::new(0),
            stream_reopens: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Verify this node has a row in the `nodes` registry.
    ///
    /// `occurrences.origin_node_id` references `nodes(node_id)`, so an enrolled
    /// node is a precondition for storing anything. Without this check the
    /// operator sees one foreign-key error per scan event instead of a single
    /// message naming the fix, which is what an unenrolled node otherwise looks
    /// like (`app ca ca-enroll` issues the credential *and* writes the registry row).
    ///
    /// A failed *lookup* is only logged: this cannot tell "not registered" apart
    /// from "database unreachable", and the storage path reports connectivity
    /// problems far more precisely.
    async fn ensure_node_registered(&self) -> Result<()> {
        match NodeRepository::is_registered(self.pool.as_pool(), self.identity.node_id()).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(AppError::NodeNotRegistered(format!(
                "{} has no row in the nodes table, so its occurrences would be rejected by the \
                 origin_node_id foreign key. Enroll it against this node's database with:\n  \
                 app ca ca-enroll --public-key-file <data dir>/{}\nwhich reads the public half \
                 this node wrote beside its key, or copy the key itself:\n  app ca ca-enroll \
                 --public-key {}",
                hex::encode(self.identity.node_id()),
                NodeIdentity::PUBLIC_KEY_FILENAME,
                hex::encode(self.identity.verifying_key().as_bytes()),
            ))),
            Err(e) => {
                warn!("Could not verify node registration, continuing: {}", e);
                Ok(())
            }
        }
    }

    /// Run the FullNode, monitoring Bluetooth devices and storing occurrences.
    ///
    /// This method runs until the process is terminated (e.g., Ctrl+C).
    /// Device event streams are not guaranteed to stay open for the life of
    /// the process: a backend's stream ends when its event source closes
    /// (Bluetooth stack restart, D-Bus disconnect, adapter reset, or a
    /// short-lived/placeholder stream). Rather than shutting the node down
    /// on such a non-error close, a supervising loop waits
    /// [`FullNodeConfig::stream_reopen_delay_ms`] and reopens a fresh
    /// stream (restarting the scan along the way).
    ///
    /// # Arguments
    ///
    /// * `monitor` - A Bluetooth device monitor (must implement DeviceMonitor)
    ///
    /// # Returns
    ///
    /// * `Err(AppError)` - If an unrecoverable error occurred (adapter
    ///   unavailable, initial scan failure, or the node is not enrolled)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::full::{FullNode, FullNodeConfig};
    /// use bt_mon::create_btleplug_monitor;
    ///
    /// let config = FullNodeConfig::new(pool, data_dir);
    /// let node = FullNode::new(config).await?;
    /// let monitor = create_btleplug_monitor().await?;
    ///
    /// node.run(monitor).await?;
    /// ```
    pub async fn run(&self, monitor: &mut (dyn DeviceMonitor + Send + Sync)) -> Result<()> {
        info!("Starting Bluetooth monitoring...");

        self.ensure_node_registered().await?;

        // Keep the CA's list current on its own task rather than alongside the
        // Bluetooth events. The two fail independently: a radio outage silences the
        // event stream while the list keeps ageing, so a node that only refreshed
        // while its radio was healthy would lose its revocation knowledge exactly
        // when it was busiest retrying the adapter.
        if let Some(watch) = self.revocation.clone() {
            tokio::spawn(async move {
                // The watch was loaded during startup, so the first refresh is due
                // one interval from now, not immediately.
                let every = watch.refresh_interval();
                let mut ticker =
                    tokio::time::interval_at(tokio::time::Instant::now() + every, every);
                loop {
                    ticker.tick().await;
                    if let Err(e) = watch.refresh().await {
                        // The previous list stays in place: it is still the newest
                        // thing the CA actually signed, and its own age — not this
                        // failure — decides how far it can be trusted. A CA that
                        // stays unreachable past that bound stops the node recording
                        // through the data policy, which is the only reason a
                        // refresh failure should ever cost data.
                        warn!(
                            "Revocation list refresh failed, keeping list #{}: {e}",
                            watch.sequence_number()
                        );
                    }
                }
            });
        }

        // Check if adapter is powered
        let powered = monitor.is_powered().await.map_err(AppError::Bluetooth)?;
        if !powered {
            warn!("Bluetooth adapter is not powered on");
        } else {
            info!("Bluetooth adapter is powered on");
        }

        // Start scanning
        monitor.start_scan().await.map_err(AppError::Bluetooth)?;
        info!("Scan started");

        let reopen_delay = Duration::from_millis(self.stream_reopen_delay_ms);
        // How often to look for devices whose window elapsed without a report. A
        // quarter of the window puts a re-observation at most that far behind the
        // window it belongs to, floored so a small window cannot turn the node's
        // main loop into a busy poll, and capped so a large one still samples on
        // time rather than a quarter of a window late.
        let sampling_tick =
            (self.sampling.window() / 4).clamp(MIN_SAMPLING_TICK, MAX_SAMPLING_TICK);
        info!(
            "Listening for Bluetooth events (press Ctrl+C to stop; a closed event stream will be reopened after {:?})",
            reopen_delay
        );
        info!(
            "Sampling window {:?}, presence trusted for {:?}, re-observation check every {:?}",
            self.sampling.window(),
            self.sampling.presence_timeout(),
            sampling_tick
        );

        let mut reopens = 0usize;
        loop {
            if reopens > 0 {
                // The previous stream ended without error. Wait before
                // reconnecting so a backend whose stream closes immediately
                // (or a flapping adapter) does not cause a tight loop.
                info!(
                    "Device event stream closed; reopening in {:?} ...",
                    reopen_delay
                );
                tokio::time::sleep(reopen_delay).await;

                // The adapter-side scan state is likely stale (e.g. the
                // Bluetooth stack restarted), so re-establish it
                // defensively. Both calls are best-effort: a scan that is
                // already dead or an adapter that is powered off is not
                // fatal here.
                if let Err(e) = monitor.stop_scan().await {
                    debug!("stop_scan before stream reopen: {}", e);
                }
                if let Err(e) = monitor.start_scan().await {
                    warn!("Failed to restart scan after stream close: {}", e);
                }
            }

            let mut events = match monitor.device_events().await {
                Ok(stream) => stream,
                Err(e) => {
                    // Opening the stream failed: not a stream close, but
                    // keep the node alive and retry with the same delay.
                    reopens += 1;
                    self.stream_reopens
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    warn!(
                        "Failed to open device event stream ({}); retrying in {:?} ...",
                        e, reopen_delay
                    );
                    tokio::time::sleep(reopen_delay).await;
                    continue;
                }
            };

            // Consume events until the stream closes. The sampling tick runs
            // alongside them: a backend that reports a device only when something
            // changes would otherwise leave a stationary device recorded once and
            // then never again, which is the defect this closes. The rate at which
            // present devices are observed is the node's to guarantee, not the
            // radio's to provide.
            let mut ticker = tokio::time::interval(sampling_tick);
            loop {
                let event = tokio::select! {
                    event = events.next() => event,
                    _ = ticker.tick() => {
                        self.sample_present_devices(&*monitor).await;
                        continue;
                    }
                };

                let Some(event) = event else {
                    break;
                };

                self.total_events
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                match event {
                    DeviceEvent::DeviceAdded { device } => {
                        info!("Discovered device: {}", device.id);
                        self.observe(&device).await;
                    }

                    DeviceEvent::DeviceRemoved { id } => {
                        debug!("Device removed: {}", id);
                        self.handle_absence(&id);
                    }

                    DeviceEvent::DeviceUpdated {
                        device,
                        changed_fields,
                    } => {
                        debug!(
                            "Device updated: {} (changed: {:?})",
                            device.id, changed_fields
                        );
                        self.observe(&device).await;
                    }

                    // The radio reported the device and nothing tracked about it
                    // moved. This is the bulk of what a radio says, and the reason
                    // the record rate is the window rather than the event rate:
                    // it is presence, not news.
                    DeviceEvent::Advertisement { device } => {
                        self.observe(&device).await;
                    }
                }
            }

            // The stream completed (non-error close). Keep the node alive by
            // looping back to reopen it.
            reopens += 1;
            self.stream_reopens
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Handle one report about a device, whatever kind of report it was.
    ///
    /// Every event that says a device is here arrives here — discovery, a property
    /// change, an advertisement that changed nothing — and the sampling policy
    /// decides whether it is worth a row. That is the whole of the fix for the
    /// record rate tracking signal jitter: the node no longer sorts reports by
    /// which field moved and stores only the ones it finds interesting.
    async fn observe(&self, device: &BluetoothDevice) {
        let identity = derive_device_identity(device.id.as_str());
        if !identity.mac_derived() {
            NON_MAC_IDENTIFIER_WARNED.call_once(|| {
                    warn!(
                        "Backend identifier '{}' is not a MAC address: storing NULL device_address and a \
                         host-local device_hash that will not match occurrences of the same device seen by other nodes",
                        device.id
                    );
                });
        }

        let content = advertisement_content(device, self.store_raw_payload);
        match self
            .sampling
            .sighting(&identity.hash, device.id.as_str(), &content)
        {
            Decision::Record(reason) => {
                debug!(
                    "Recording {} for {} ({})",
                    reason.label(),
                    device.id,
                    content.summary()
                );
                if let Err(e) = self.store_occurrence(device, &identity).await {
                    error!("Error storing occurrence: {}", e);
                    self.storage_errors
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Decision::Suppress { reason, until } => {
                debug!(
                    "Not storing {}: {:?}, eligible again in {:?}",
                    device.id, reason, until
                );
                self.occurrences_rate_limited
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    /// Handle a device the backend reports absent.
    ///
    /// The window goes with it. The interval a window measures is the co-presence
    /// that just ended, not the one that starts if the device is back in two
    /// seconds — and that return is worth a row, which is exactly what a window
    /// left running would prevent.
    fn handle_absence(&self, id: &DeviceId) {
        self.sampling
            .removed(&derive_device_identity(id.as_str()).hash);
    }

    /// Re-observe the devices the policy says are due, and expire the ones that
    /// stopped being reported.
    ///
    /// This is the lower bound of the policy — the reason a stationary beacon
    /// yields a row per window instead of one per visit — and it is a *read*, not a
    /// replay. The monitor is asked what it knows about the device now; the node
    /// never writes a cached snapshot with a fresh timestamp, because
    /// `observed_at` claims the signal was observed then. A device the monitor no
    /// longer has is dropped from presence and counted, so the absence shows up as
    /// the absence it is.
    async fn sample_present_devices(&self, monitor: &(dyn DeviceMonitor + Send + Sync)) {
        let expired = self.sampling.prune();
        if expired > 0 {
            debug!("Presence expired for {expired} devices");
        }

        for (hash, reported_id) in self.sampling.due() {
            let id = DeviceId::new(reported_id);
            let device = match monitor.device(&id).await {
                Ok(device) => device,
                Err(e) => {
                    // No reading means nothing to observe, and the node cannot
                    // tell "left range" from "backend forgot it" — either way this
                    // node stops claiming it is present.
                    self.sampling.reobservation_failed(&hash);
                    debug!(
                        "Dropped {} from presence, monitor has no reading: {}",
                        id, e
                    );
                    continue;
                }
            };

            let content = advertisement_content(&device, self.store_raw_payload);
            // The claim is taken here rather than trusted from `due`: an
            // advertisement may have recorded this device while the monitor was
            // being read, and two rows in one window is the bound that broke.
            if !self.sampling.reobservation(&hash, &content).is_record() {
                continue;
            }

            let identity = derive_device_identity(device.id.as_str());
            debug!("Re-observing {}", device.id);
            if let Err(e) = self.store_occurrence(&device, &identity).await {
                error!("Error storing re-observation: {}", e);
                self.storage_errors
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    /// Store an occurrence for a device whose report the sampling policy already
    /// accepted.
    ///
    /// This method:
    /// 1. Normalizes the backend device identifier into an address and device hash
    /// 2. Checks revocation status (if enabled)
    /// 3. Builds a canonical payload
    /// 4. CBOR encodes the payload
    /// 5. Signs the encoded bytes
    /// 6. Inserts the occurrence into the database
    ///
    /// It does **not** decide whether a row is owed. That is
    /// [`ObservationPolicy`]'s call, and it has to be made once, above both paths
    /// that get here: an occurrence written without the policy having claimed the
    /// window would silently break the upper bound, because the policy would go on
    /// believing the device was recorded at the time it last said so.
    ///
    /// Backends that do not expose a MAC address (CoreBluetooth reports a
    /// host-local UUID) store a NULL `device_address`; the identifier is still
    /// covered by `device_hash`, and `signal_payload` records how it was derived.
    ///
    /// # Arguments
    ///
    /// * `device` - The device to store
    /// * `identity` - Its derived identity, from the same identifier the policy
    ///   decided on
    ///
    /// # Returns
    ///
    /// * `Ok(())` - The occurrence was stored
    /// * `Err(AppError)` - If storage failed
    async fn store_occurrence(
        &self,
        device: &BluetoothDevice,
        identity: &DeviceIdentity,
    ) -> Result<()> {
        // Ask the CA before building anything, not after. The work below acquires a
        // position, encodes the payload and signs it, and a refusal throws all of
        // that away — but more to the point, this is the last moment where dropping
        // the occurrence is cheap: after this there is a row, and a row is exactly
        // what a revoked node is trying to produce.
        //
        // Received occurrences from a peer are gated separately, by
        // verify_received_occurrence(); this path is the node's own observations.
        self.authorize_store(self.identity.node_id())?;

        // Generate timestamps. One clock read for both columns: the pair is the
        // drift audit's raw material, and two reads would record the gap between
        // them rather than any drift.
        let _occurrence_id = Uuid::now_v7();
        let (observed_at, observed_at_node_local) = self.clock.now_pair();

        // Where the node says it was. A failure here is logged and treated as
        // "no location": losing a location is survivable, dropping the occurrence
        // is not.
        let position = self.position.locate().await;

        // Build signal payload with Bluetooth-specific data
        let mut signal_payload = ble_signal_payload(device, &identity, self.store_raw_payload);
        if let Some(position) = position.as_ref() {
            record_position(&mut signal_payload, position);
        }

        let occurrence = signed_occurrence(
            &self.identity,
            device,
            &identity,
            observed_at,
            observed_at_node_local,
            position.as_ref(),
            signal_payload,
        )?;

        // Insert into database
        match OccurrenceRepository::create(self.pool.as_pool(), &occurrence).await {
            Ok(_) => {
                self.occurrences_stored
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                debug!("Stored occurrence for device: {}", device.id);
            }
            Err(e) => {
                // Check if it's a duplicate (shouldn't happen with UUIDv7)
                error!("Failed to store occurrence: {}", e);
                return Err(AppError::Database(e));
            }
        }

        Ok(())
    }

    /// Get the node's unique ID.
    ///
    /// This is a 32-byte SHA-256 hash of the signing public key.
    pub fn node_id(&self) -> &[u8] {
        self.identity.node_id()
    }

    /// Get statistics about node operation.
    pub fn stats(&self) -> FullNodeStats {
        FullNodeStats {
            total_events: self.total_events.load(std::sync::atomic::Ordering::SeqCst),
            occurrences_stored: self
                .occurrences_stored
                .load(std::sync::atomic::Ordering::SeqCst),
            occurrences_rate_limited: self
                .occurrences_rate_limited
                .load(std::sync::atomic::Ordering::SeqCst),
            storage_errors: self
                .storage_errors
                .load(std::sync::atomic::Ordering::SeqCst),
            stream_reopens: self
                .stream_reopens
                .load(std::sync::atomic::Ordering::SeqCst),
            sampling: self.sampling.stats(),
            rate_limiter_stats: self.sampling.rate_limiter_stats(),
        }
    }

    /// Get the rate limit threshold in milliseconds.
    pub fn rate_limit_threshold_ms(&self) -> u64 {
        self.sampling.window().as_millis() as u64
    }

    /// Clear the node's sampling state: every device's window and presence.
    ///
    /// This is primarily useful for testing. A node that forgets its windows
    /// records every device in range again on its next report for it.
    pub fn clear_rate_limiter(&self) {
        self.sampling.clear();
    }

    /// Verify a signature from this node.
    ///
    /// # Arguments
    ///
    /// * `payload` - The bytes that were signed
    /// * `signature` - The signature to verify
    ///
    /// # Returns
    ///
    /// * `Ok(())` - The signature is valid
    /// * `Err(VerifyError)` - If verification fails
    pub fn verify(&self, payload: &[u8], signature: &ed25519_dalek::Signature) -> Result<()> {
        self.identity
            .verify(payload, signature)
            .map_err(|e| AppError::Validation(e.to_string()))
    }

    /// The revocation gate for anything this node is about to record.
    ///
    /// Checking off is the operator's decision not to ask the CA, so it is a pass,
    /// not a refusal: the absence of a gate must not be mistaken for a gate that
    /// said no. Checking on is the CA's answer, and a refusal there is an error the
    /// caller has to surface — an occurrence that was observed and then dropped is
    /// the interesting failure, not a silent `false`.
    ///
    /// # Self-attestation
    ///
    /// A node checks its *own* id here, which reads oddly until you notice who it
    /// protects against: not this process, which can bypass anything it holds, but
    /// a revocation issued against this key and a deployment that was never told. A
    /// node whose key has been revoked by its own CA stops recording the moment it
    /// reloads the list, which is the behaviour an operator would assume.
    fn authorize_store(&self, node_id: &[u8]) -> Result<()> {
        match &self.revocation {
            Some(watch) => watch.authorize_data(node_id),
            None => Ok(()),
        }
    }

    /// Verify a received occurrence from another node.
    ///
    /// Nothing calls this yet — the node has no P2P transport, so there is no
    /// receive handler to call it from. It exists for the day one does, and it is
    /// written for that day rather than for the one before it: a peer's signature
    /// is checked against **that peer's** key as the registry holds it, never
    /// against this node's own key (which is what an earlier version did, and which
    /// would have accepted exactly nothing while looking like authentication).
    ///
    /// It verifies:
    /// 1. The peer is enrolled, so there is a registered key to check against.
    /// 2. That key hashes to the node id the occurrence claims — otherwise the
    ///    registry row says nothing about the id being asserted, and a signature
    ///    under it proves possession of some key, not this node's.
    /// 3. The signature is valid under that key.
    /// 4. The CA's list permits recording data from that node
    ///    ([`Self::authorize_store`]).
    ///
    /// What it does *not* establish is that the CA issued the registered key. The
    /// registry row's `ca_credential` column holds exactly that statement and can be
    /// checked with [`ca::TrustAnchor`] the way `ca ca-verify-credential` does; this
    /// cannot, because the anchor is configured under `[revocation]` and
    /// authentication must not silently depend on a revocation switch. Wiring the
    /// credential check is GAP_ANALYSIS M14's remaining half and needs the anchor
    /// promoted to its own setting.
    ///
    /// # Arguments
    ///
    /// * `origin_node_id` - The ID of the node that created the occurrence
    /// * `signed_payload` - The bytes that were signed
    /// * `signature` - The signature to verify
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Signature is valid and the node is not revoked
    /// * `Err(AppError)` - If verification or revocation check failed
    pub async fn verify_received_occurrence(
        &self,
        origin_node_id: &[u8],
        signed_payload: &[u8],
        signature: &[u8],
    ) -> Result<()> {
        let peer = NodeRepository::find_by_id(self.pool.as_pool(), origin_node_id)
            .await
            .map_err(|e| {
                AppError::Validation(format!(
                    "could not look up reporting node {}: {e}",
                    hex::encode(origin_node_id)
                ))
            })?
            .ok_or_else(|| {
                AppError::Provenance(format!(
                    "reporting node {} is not enrolled, so there is no registered key to verify \
                     its signature against",
                    hex::encode(origin_node_id)
                ))
            })?;

        let peer_key = ed25519_dalek::VerifyingKey::try_from(peer.signing_public_key.as_slice())
            .map_err(|e| {
                AppError::Validation(format!(
                    "the registered signing key for {} is not a valid Ed25519 public key: {e}",
                    hex::encode(origin_node_id)
                ))
            })?;

        // The id *is* SHA-256 of the key, so this is what ties the row to the
        // identity being claimed. A registry row that fails it is corrupt or
        // planted, and verifying anything against it would be theatre.
        let derived = <[u8; 32]>::from(sha2::Sha256::digest(peer_key.as_bytes()));
        if derived.as_slice() != origin_node_id {
            return Err(AppError::Provenance(format!(
                "the signing key registered for {} hashes to {}, so it is not that node's key",
                hex::encode(origin_node_id),
                hex::encode(derived)
            )));
        }

        peer_key
            .verify_strict(
                signed_payload,
                &ed25519_dalek::Signature::try_from(signature)
                    .map_err(|e| AppError::Validation(format!("Invalid signature format: {e}")))?,
            )
            .map_err(|e| {
                AppError::Provenance(format!(
                    "signature from {} does not verify under its registered key: {e}",
                    hex::encode(origin_node_id)
                ))
            })?;

        self.authorize_store(origin_node_id)
    }

    /// Check if a peer node should be allowed to connect (P2P handshake).
    ///
    /// Nothing calls this either — there is no handshake to hook it into. With
    /// checking enabled it asks the CA through the connection policy; with checking
    /// disabled it allows the peer, because "no gate is configured" is a different
    /// statement from "the gate said no", and answering `false` to every peer was
    /// the old behaviour's actual effect while reading as a security decision.
    ///
    /// This is stricter than data recording while checking *is* enabled: a node the
    /// list does not name is refused a session but still recorded, on the ground
    /// that one more row from an unestablished node costs a row, while admitting it
    /// to a session costs everything that session touches.
    ///
    /// # Arguments
    ///
    /// * `peer_node_id` - The ID of the connecting node
    ///
    /// # Returns
    ///
    /// * `Ok(true)` - Node should be allowed to connect
    /// * `Ok(false)` - Node should be rejected
    /// * `Err(AppError)` - If check failed
    pub fn should_allow_peer_connection(&self, peer_node_id: &[u8]) -> Result<bool> {
        match &self.revocation {
            Some(watch) => Ok(watch.authorize_connection(peer_node_id)),
            None => Ok(true),
        }
    }
}

impl Node for FullNode {
    fn node_id(&self) -> &[u8] {
        self.identity.node_id()
    }

    fn sign(&self, payload: &[u8]) -> ed25519_dalek::Signature {
        self.identity.sign(payload)
    }

    fn verify(
        &self,
        payload: &[u8],
        signature: &ed25519_dalek::Signature,
    ) -> crate::provenance::verify::Result<()> {
        self.identity.verify(payload, signature)
    }
}

/// Build and sign the occurrence for one device observation.
///
/// A free function rather than a method so the payload/occurrence pairing — in
/// particular the way an acquired position has to reach *both* the signed bytes
/// and the row — can be tested without a database or a running node.
///
/// The row is built first and the [payload](PayloadV2) is then derived from it,
/// which is what keeps them honest: the attestation is a function of the row, so
/// there is no pair of parallel field lists for a future edit to pull apart. The
/// signature covers every column this node authors — the location and its derived
/// H3 cells, the altitude and accuracy of the fix, the advertisement contents in
/// `signal_payload` including the [`record_position`] provenance block, the device
/// name, and both timestamps at the precision the row keeps.
///
/// Three things it cannot cover, and why:
///
/// - `occurrence_id`: see the payload module docs; binding it is [GAP_ANALYSIS M21].
/// - `ingested_at`: written by whoever stores the row, not by the origin node.
/// - `adv_type` / `tx_power`: the capture layer reports neither yet, so they are
///   signed as absent. Filling them in later ([GAP_ANALYSIS M22]) without a separate
///   attestation would make the row disagree with its own signature — which is the
///   point of having signed them.
fn signed_occurrence(
    identity: &NodeIdentity,
    device: &BluetoothDevice,
    device_identity: &DeviceIdentity,
    observed_at: DateTime<Utc>,
    observed_at_node_local: DateTime<Utc>,
    position: Option<&Position>,
    signal_payload: serde_json::Value,
) -> Result<Occurrence> {
    // Both timestamps to microseconds, once, before either the row or the signature
    // is built. `TIMESTAMPTZ` stores microseconds; signing a nanosecond value would
    // sign something the column cannot show, which is the reason row ↔ proof
    // cross-checking was structurally impossible before v2.
    let observed_at = truncate_to_micros(observed_at);
    let observed_at_node_local = truncate_to_micros(observed_at_node_local);

    let rssi = device.rssi.unwrap_or(0) as i16;

    let mut occurrence_builder = Occurrence::builder()
        .signal_type(SignalType::Bluetooth)
        .origin_node_id(identity.node_id())
        .observed_at(observed_at)
        .observed_at_node_local(observed_at_node_local)
        .device_hash(&device_identity.hash)
        .rssi(rssi)
        .signal_payload(signal_payload);
    if let Some(address) = device_identity.address.as_deref() {
        occurrence_builder = occurrence_builder.device_address(address);
    }
    if let Some(name) = device.name.as_deref() {
        occurrence_builder = occurrence_builder.advertised_name(name);
    }
    occurrence_builder = match position {
        Some(position) => occurrence_builder.with_location(
            position.latitude,
            position.longitude,
            position.altitude_m.map(|m| m as f32),
            position.accuracy_m.map(|m| m as f32),
            position.location_source(),
        ),
        // No fix: the column is NOT NULL and still gets a label. Stated here rather
        // than left to the model's default, because the signature now attests to it.
        None => occurrence_builder.location_source(LocationSource::NodeGps),
    };

    let mut occurrence = occurrence_builder.build();

    let payload = PayloadV2::from_occurrence(&occurrence)
        .map_err(|e| AppError::Validation(format!("Canonical payload failed: {e}")))?;
    let encoded = encode_payload(&payload.into())
        .map_err(|e| AppError::Validation(format!("CBOR encoding failed: {}", e)))?;
    let signature = identity.sign(&encoded);

    occurrence.signed_payload = encoded;
    occurrence.signature = signature.to_bytes().to_vec();
    // The column mirrors the version the document declares, so a reader can tell
    // which shape it holds without decoding it.
    occurrence.schema_version =
        i16::try_from(CURRENT_VERSION).expect("the payload version fits the SMALLINT column");

    Ok(occurrence)
}

/// Record how the asserted location was obtained.
///
/// The `location_source` column can only say which bucket a fix fell into. This
/// keeps the distinctions it cannot hold: a simulated fix is stored as
/// `node_gps` because that is what it stands in for, and the `mock` label lives
/// here so test data stays separable from a real receiver's without inventing an
/// enum variant. `fixed_at` distinguishes a fresh fix from one the cache served.
fn record_position(payload: &mut serde_json::Value, position: &Position) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };

    map.insert(
        "position".to_string(),
        serde_json::json!({
            "origin": position.origin.as_str(),
            "fixed_at": position.fixed_at.to_rfc3339(),
            "accuracy_m": position.accuracy_m,
            "altitude_m": position.altitude_m,
        }),
    );
}

/// What a device is advertising, reduced to what the sampling policy compares.
///
/// RSSI is not in here, nor the connection state, nor whether services have been
/// resolved. Those describe this node's reading and its GATT usage, not what the
/// device put on the air, and counting them as advertisement content is how a node
/// came to record a device at whatever rate its signal happened to wobble.
///
/// `store_raw_payload` gates the radio's own bytes because the digest is a claim
/// about what a row would contain: bytes the operator asked not to keep are not
/// news about the advertisement.
///
/// A free function so that what counts as a change can be tested without a
/// database or a running node.
fn advertisement_content(
    device: &BluetoothDevice,
    store_raw_payload: bool,
) -> AdvertisementContent {
    let manufacturer_data: BTreeMap<u16, Vec<u8>> = device
        .manufacturer_data
        .iter()
        .map(|(company, bytes)| (*company, bytes.clone()))
        .collect();
    let service_data: BTreeMap<String, Vec<u8>> = device
        .service_data
        .iter()
        .map(|(uuid, bytes)| (uuid.to_string(), bytes.clone()))
        .collect();

    AdvertisementContent::new(
        device.name.as_deref(),
        &manufacturer_data,
        &service_data,
        if store_raw_payload {
            device.raw_payload.as_deref()
        } else {
            None
        },
    )
}

/// Build the Bluetooth-specific `signal_payload` for one observation.
///
/// A free function rather than a method so the shape of what gets stored — in
/// particular what `store_raw_payload` adds and withholds — can be tested without
/// a database or a running node.
///
/// `id_source` and `mac_derived` record where the identifier came from, so
/// downstream queries can tell a host-local hash apart from a globally
/// comparable MAC-derived one.
///
/// `store_raw_payload` decides whether the radio's own advertisement bytes are
/// kept alongside the fields parsed out of them. They are the only part of a
/// capture that cannot be recovered later: the parser will learn new
/// advertisement structures, and an advertisement that arrived today and was
/// discarded cannot be re-read then. Whatever is inserted here is covered by the
/// signature, because `signal_payload` is part of the signed payload.
fn ble_signal_payload(
    device: &BluetoothDevice,
    identity: &DeviceIdentity,
    store_raw_payload: bool,
) -> serde_json::Value {
    let mut ble_payload = serde_json::Map::new();

    // Identifier provenance
    ble_payload.insert(
        "id_source".to_string(),
        serde_json::json!(identity.source.as_str()),
    );
    ble_payload.insert(
        "mac_derived".to_string(),
        serde_json::json!(identity.mac_derived()),
    );

    // Address type (default to public) — unknown when the backend hides the
    // MAC address, so it is not guessed for those identifiers.
    ble_payload.insert(
        "address_type".to_string(),
        if identity.mac_derived() {
            serde_json::json!("public")
        } else {
            serde_json::Value::Null
        },
    );

    // Device address as hex string (null when the backend exposes no MAC)
    ble_payload.insert(
        "address".to_string(),
        match identity.address.as_deref() {
            Some(address) => serde_json::json!(hex::encode(address)),
            None => serde_json::Value::Null,
        },
    );

    // The advertisement as the radio delivered it, when the operator asked for it
    // and this backend can produce it. A `None` here means the backend does not
    // expose advertisement bytes at all — never that the device advertised
    // nothing, which would be an empty `Some`.
    if store_raw_payload {
        match device.raw_payload.as_deref() {
            Some(bytes) => {
                ble_payload.insert(
                    "raw_payload_hex".to_string(),
                    serde_json::json!(hex::encode(bytes)),
                );
            }
            None => RAW_PAYLOAD_ABSENT_WARNED.call_once(|| {
                warn!(
                    "store_raw_payload is on, but the backend reports no advertisement bytes for \
                     '{}': occurrences will carry the parsed fields only. btleplug and bluer hand \
                     over decoded properties, not the advertisement structures themselves.",
                    device.id
                );
            }),
        }
    }

    // Handle manufacturer data
    if let Some((company_id, payload)) = device.manufacturer_data.iter().next() {
        ble_payload.insert(
            "manufacturer_data".to_string(),
            serde_json::json!({
                "company_id": company_id,
                "payload": hex::encode(payload)
            }),
        );
    }

    // Handle service UUIDs
    if !device.service_data.is_empty() {
        let uuids: Vec<String> = device
            .service_data
            .keys()
            .map(|u| u.as_uuid().to_string())
            .collect();
        ble_payload.insert("service_uuids".to_string(), serde_json::json!(uuids));
    }

    // Add RSSI if available
    if let Some(rssi) = device.rssi {
        ble_payload.insert("rssi".to_string(), serde_json::json!(rssi));
    }

    // Add device name if available
    if let Some(name) = &device.name {
        ble_payload.insert("name".to_string(), serde_json::json!(name));
    }

    // Add services resolved flag
    ble_payload.insert(
        "services_resolved".to_string(),
        serde_json::json!(device.services_resolved),
    );

    // Wrap in signal_type key
    let mut signal_payload = serde_json::Map::new();
    signal_payload.insert("ble".to_string(), serde_json::json!(ble_payload));

    serde_json::Value::Object(signal_payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::test_support::{epoch, ManualClock};
    use crate::position::PositionOrigin;
    use crate::provenance::encode::{canonical_signal_payload, decode_payload};
    use crate::provenance::payload::{
        canonical_timestamp, location_source_label, CanonicalPayload, PayloadV1, VERSION_V1,
    };

    fn test_identity() -> NodeIdentity {
        // The temp dir only has to survive the load: the key pair lives in the
        // returned identity afterwards.
        let dir = tempfile::tempdir().unwrap();
        NodeIdentity::load_or_create(dir.path()).expect("identity in a temp dir")
    }

    fn observed_device() -> BluetoothDevice {
        BluetoothDevice::new(
            bt_mon::DeviceId::new("AA:BB:CC:DD:EE:FF"),
            "AA:BB:CC:DD:EE:FF".to_string(),
        )
    }

    fn build(identity: &NodeIdentity, position: Option<&Position>) -> Occurrence {
        build_from(
            identity,
            &observed_device(),
            position,
            epoch(),
            epoch(),
            serde_json::json!({ "ble": {} }),
        )
    }

    /// One occurrence as this node would write it, with the caller's inputs and
    /// nothing else varying, so a difference between two of them is attributable.
    fn build_from(
        identity: &NodeIdentity,
        device: &BluetoothDevice,
        position: Option<&Position>,
        observed_at: DateTime<Utc>,
        observed_at_node_local: DateTime<Utc>,
        signal_payload: serde_json::Value,
    ) -> Occurrence {
        let device_identity = derive_device_identity(device.id.as_str());

        signed_occurrence(
            identity,
            device,
            &device_identity,
            observed_at,
            observed_at_node_local,
            position,
            signal_payload,
        )
        .expect("building an occurrence should not fail")
    }

    fn fix(origin: PositionOrigin) -> Position {
        Position::with_detail(40.6892, -74.0445, Some(12.5), Some(4.0), origin, epoch()).unwrap()
    }

    /// The same fix with one detail changed.
    fn fix_detail(altitude_m: f64, accuracy_m: f64) -> Position {
        Position::with_detail(
            40.6892,
            -74.0445,
            Some(altitude_m),
            Some(accuracy_m),
            PositionOrigin::Gps,
            epoch(),
        )
        .unwrap()
    }

    fn beacon(name: &str, rssi: i32) -> BluetoothDevice {
        observed_device().with_name(name).with_rssi(rssi)
    }

    fn signed(occurrence: &Occurrence) -> CanonicalPayload {
        decode_payload(&occurrence.signed_payload)
            .expect("the stored bytes are a payload this build can read")
    }

    #[test]
    fn an_acquired_position_lands_in_the_row_and_in_the_signed_bytes() {
        let occurrence = build(&test_identity(), Some(&fix(PositionOrigin::Gps)));

        let point = occurrence
            .location
            .as_ref()
            .expect("location should be stored");
        // Stored as (lon, lat): the geography column is x/east, y/north.
        assert_eq!(point.0.x(), -74.0445);
        assert_eq!(point.0.y(), 40.6892);
        assert_eq!(occurrence.alt_m, Some(12.5));
        assert_eq!(occurrence.accuracy_m, Some(4.0));
        assert_eq!(occurrence.location_source, LocationSource::NodeGps);

        assert_eq!(signed(&occurrence).location(), Some([40.6892, -74.0445]));
    }

    #[test]
    fn a_node_with_no_position_stores_no_location() {
        let occurrence = build(&test_identity(), None);

        assert!(occurrence.location.is_none());
        assert_eq!(signed(&occurrence).location(), None);
    }

    #[test]
    fn the_signature_covers_the_location() {
        let identity = test_identity();
        // Same observation, different place: were the location only in the row, the
        // signed bytes and therefore the signature would be identical.
        let here = build(&identity, Some(&fix(PositionOrigin::Gps)));
        let moved = Position::new(41.0, -74.0445, PositionOrigin::Gps, epoch()).unwrap();
        let elsewhere = build(&identity, Some(&moved));
        let nowhere = build(&identity, None);

        assert_ne!(here.signed_payload, elsewhere.signed_payload);
        assert_ne!(here.signature, elsewhere.signature);
        assert_ne!(here.signed_payload, nowhere.signed_payload);

        for occurrence in [&here, &elsewhere, &nowhere] {
            let signature = ed25519_dalek::Signature::try_from(occurrence.signature.as_slice())
                .expect("64-byte signature");
            identity
                .verify(&occurrence.signed_payload, &signature)
                .expect("the stored signature must verify over the stored payload");
        }
    }

    #[test]
    fn the_signature_names_every_column_the_node_authors() {
        let identity = test_identity();
        let device = beacon("Liberty Beacon", -63);
        let occurrence = build_from(
            &identity,
            &device,
            Some(&fix(PositionOrigin::Gps)),
            epoch(),
            epoch(),
            serde_json::json!({"ble": {"name": "Liberty Beacon"}}),
        );

        let CanonicalPayload::V2(payload) = signed(&occurrence) else {
            panic!("a node running this build writes v2");
        };
        assert!(payload.geo_cell_fine.is_some());

        // Field for field, the attestation says what the row says. This is the
        // cross-check that did not exist before v2: a reader holding only the row and
        // the node's key can now compare the two for every column the node wrote.
        assert_eq!(payload.signal_type, 0);
        assert_eq!(payload.origin_node_id, identity.node_id());
        assert_eq!(
            payload.device_hash,
            derive_device_identity(device.id.as_str()).hash
        );
        assert_eq!(
            payload.device_address.as_deref(),
            occurrence.device_address.as_deref()
        );
        assert_eq!(payload.rssi, occurrence.rssi);
        assert_eq!(payload.rssi, -63);
        assert_eq!(payload.advertised_name, occurrence.advertised_name);
        assert_eq!(payload.advertised_name.as_deref(), Some("Liberty Beacon"));
        assert_eq!(payload.location, Some([40.6892, -74.0445]));
        assert_eq!(payload.alt_m, occurrence.alt_m);
        assert_eq!(payload.accuracy_m, occurrence.accuracy_m);
        // The wire carries a code and the column holds a label, so the comparison goes
        // through the table: a code assigned to the wrong variant would be caught here
        // rather than at the point where a row is rejected for a value that cannot exist.
        assert_eq!(
            location_source_label(payload.location_source),
            Some(occurrence.location_source.as_str())
        );
        assert_eq!(
            payload.signal_payload,
            canonical_signal_payload(&occurrence.signal_payload).unwrap()
        );

        // The cells the generated columns will hold, derived the way the server
        // derives them — repo::geo and h3-pg agreeing is pinned by repo's wire tests.
        let point = occurrence.location.as_ref().unwrap();
        let (lat, lon) = (point.0.y(), point.0.x());
        assert_eq!(
            payload.geo_cell_fine,
            Some(u64::from(repo::geo::fine_cell(lat, lon).unwrap()))
        );
        assert_eq!(
            payload.geo_cell_macro,
            Some(u64::from(repo::geo::macro_cell(lat, lon).unwrap()))
        );

        // The two columns the capture layer cannot report yet: signed as absent, and
        // the row agrees that they are absent.
        assert_eq!(payload.adv_type, None);
        assert_eq!(occurrence.adv_type, None);
        assert_eq!(payload.tx_power, occurrence.tx_power);
        assert_eq!(occurrence.tx_power, None);

        // And the row declares the version its bytes carry.
        assert_eq!(
            occurrence.schema_version,
            i16::try_from(CURRENT_VERSION).unwrap()
        );
        assert_eq!(payload.schema_version, CURRENT_VERSION);
    }

    #[test]
    fn every_column_the_node_authors_changes_the_signature() {
        // The point of v2, stated as a test: for each of these columns, a row whose
        // value differs has to differ in its signed bytes and in its signature. Under
        // v1 every case after the first four produced identical bytes, so editing the
        // column was undetectable.
        let identity = test_identity();
        let base = build_from(
            &identity,
            &beacon("Beacon A", -60),
            Some(&fix(PositionOrigin::Gps)),
            epoch(),
            epoch(),
            serde_json::json!({"ble": {"name": "Beacon A"}}),
        );
        let micros = chrono::Duration::microseconds(1);

        let variants: Vec<(&str, Occurrence)> = vec![
            (
                "advertised_name",
                build_from(
                    &identity,
                    &beacon("Beacon B", -60),
                    Some(&fix(PositionOrigin::Gps)),
                    epoch(),
                    epoch(),
                    serde_json::json!({"ble": {"name": "Beacon B"}}),
                ),
            ),
            (
                "rssi",
                build_from(
                    &identity,
                    &beacon("Beacon A", -61),
                    Some(&fix(PositionOrigin::Gps)),
                    epoch(),
                    epoch(),
                    serde_json::json!({"ble": {"name": "Beacon A"}}),
                ),
            ),
            (
                "location",
                build(
                    &identity,
                    Some(&Position::new(41.0, -74.0445, PositionOrigin::Gps, epoch()).unwrap()),
                ),
            ),
            ("alt_m", build(&identity, Some(&fix_detail(13.5, 4.0)))),
            ("accuracy_m", build(&identity, Some(&fix_detail(12.5, 5.0)))),
            (
                "location_source",
                build(&identity, Some(&fix(PositionOrigin::Fixed))),
            ),
            (
                "observed_at",
                build_from(
                    &identity,
                    &beacon("Beacon A", -60),
                    Some(&fix(PositionOrigin::Gps)),
                    epoch() + micros,
                    epoch(),
                    serde_json::json!({"ble": {"name": "Beacon A"}}),
                ),
            ),
            (
                "observed_at_node_local",
                build_from(
                    &identity,
                    &beacon("Beacon A", -60),
                    Some(&fix(PositionOrigin::Gps)),
                    epoch(),
                    epoch() + micros,
                    serde_json::json!({"ble": {"name": "Beacon A"}}),
                ),
            ),
            (
                "signal_payload",
                build_from(
                    &identity,
                    &beacon("Beacon A", -60),
                    Some(&fix(PositionOrigin::Gps)),
                    epoch(),
                    epoch(),
                    serde_json::json!({"ble": {"name": "Beacon A", "manufacturer_data": {"company_id": 76}}}),
                ),
            ),
            (
                "device_address and device_hash",
                build_from(
                    &identity,
                    &BluetoothDevice::new(
                        bt_mon::DeviceId::new("00:11:22:33:44:55"),
                        "00:11:22:33:44:55".to_string(),
                    )
                    .with_name("Beacon A")
                    .with_rssi(-60),
                    Some(&fix(PositionOrigin::Gps)),
                    epoch(),
                    epoch(),
                    serde_json::json!({"ble": {"name": "Beacon A"}}),
                ),
            ),
        ];

        for (column, variant) in variants {
            assert_ne!(
                variant.signed_payload, base.signed_payload,
                "{column} is not inside the signed bytes"
            );
            assert_ne!(
                variant.signature, base.signature,
                "{column} changes the signature"
            );
        }
    }

    #[test]
    fn a_nanosecond_clock_reading_is_signed_at_the_precision_the_row_keeps() {
        let identity = test_identity();
        let nanos = DateTime::parse_from_rfc3339("2026-09-15T12:00:00.123456789Z")
            .unwrap()
            .with_timezone(&Utc);

        let occurrence = build_from(
            &identity,
            &observed_device(),
            None,
            nanos,
            nanos,
            serde_json::json!({"ble": {}}),
        );

        // The row holds the truncated instant rather than the nanosecond value the
        // clock handed back, so the stored column and the signature name one instant.
        assert_eq!(occurrence.observed_at, truncate_to_micros(nanos));
        assert_eq!(occurrence.observed_at_node_local, truncate_to_micros(nanos));

        let CanonicalPayload::V2(payload) = signed(&occurrence) else {
            panic!("a node running this build writes v2")
        };
        assert_eq!(payload.observed_at, "2026-09-15T12:00:00.123456+00:00");
        // What a reader holding the row can reproduce from it — the property v1's
        // nanosecond spelling could never satisfy.
        assert_eq!(
            payload.observed_at,
            canonical_timestamp(occurrence.observed_at)
        );
        assert_eq!(payload.observed_at_node_local, payload.observed_at);
    }

    #[test]
    fn a_row_signed_before_v2_still_verifies_and_says_what_it_leaves_out() {
        let identity = test_identity();
        // Stand-in for a row already in a database: v1 bytes, this node's key.
        let v1 = PayloadV1::builder()
            .origin_node_id(identity.node_id())
            .device_hash(&[1u8; 32])
            .observed_at_node_local(&epoch().to_rfc3339())
            .rssi(-70)
            .build();
        let bytes = encode_payload(&CanonicalPayload::V1(v1)).unwrap();
        let signature = identity.sign(&bytes);

        let decoded = signed(&Occurrence {
            signed_payload: bytes.clone(),
            ..Default::default()
        });

        identity
            .verify(&bytes, &signature)
            .expect("a v1 signature still verifies");
        assert_eq!(decoded.version(), VERSION_V1);
        assert!(!decoded.covers_row());
        // The columns an auditor of this row cannot check, named rather than assumed.
        assert_eq!(decoded.signed_signal_payload(), None);
        assert_eq!(decoded.observed_at(), None);
        assert_eq!(decoded.advertised_name(), None);
    }

    #[test]
    fn the_origin_decides_the_stored_location_source() {
        for (origin, expected) in [
            (PositionOrigin::Fixed, LocationSource::NodeFixed),
            (PositionOrigin::Gps, LocationSource::NodeGps),
            // A simulated fix stands in for a receiver, so it is recorded as one;
            // the `mock` label survives in signal_payload instead.
            (PositionOrigin::Mock, LocationSource::NodeGps),
        ] {
            let occurrence = build(&test_identity(), Some(&fix(origin)));
            assert_eq!(occurrence.location_source, expected, "{origin:?}");
        }
    }

    #[test]
    fn the_position_provenance_is_recorded_beside_the_bluetooth_data() {
        let mut payload = serde_json::json!({ "ble": { "adv_type": "non_conn" } });
        record_position(&mut payload, &fix(PositionOrigin::Mock));

        assert_eq!(
            payload["ble"]["adv_type"], "non_conn",
            "the BLE data survives"
        );
        assert_eq!(payload["position"]["origin"], "mock");
        assert_eq!(payload["position"]["fixed_at"], epoch().to_rfc3339());
        assert_eq!(payload["position"]["accuracy_m"], 4.0);
        assert_eq!(payload["position"]["altitude_m"], 12.5);
    }

    #[test]
    fn record_position_leaves_a_non_object_payload_alone() {
        let mut payload = serde_json::json!("not a map");
        record_position(&mut payload, &fix(PositionOrigin::Gps));

        assert_eq!(payload, serde_json::json!("not a map"));
    }

    /// A device whose radio handed over advertisement structures: flags, then a
    /// complete local name, as they appear on the air.
    fn advertised(name: &str) -> BluetoothDevice {
        let mut bytes = vec![0x02, 0x01, 0x06, 0x00];
        bytes[3] = name.len() as u8 + 1;
        bytes.push(0x09);
        bytes.extend(name.as_bytes());

        observed_device().with_name(name).with_raw_payload(bytes)
    }

    fn ble(payload: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
        payload["ble"]
            .as_object()
            .expect("signal_payload is keyed by signal type")
    }

    #[test]
    fn the_radio_bytes_are_kept_when_the_operator_asks_for_them() {
        let device = advertised("Harbor Beacon");
        let identity = derive_device_identity(device.id.as_str());

        let payload = ble_signal_payload(&device, &identity, true);

        assert_eq!(
            ble(&payload)["raw_payload_hex"],
            serde_json::json!(hex::encode(device.raw_payload.as_deref().unwrap()))
        );
        // Keeping the bytes is additive: everything the parser already extracted
        // has to stay where the queries that read it expect it.
        for key in [
            "id_source",
            "mac_derived",
            "address_type",
            "address",
            "name",
        ] {
            assert!(ble(&payload).contains_key(key), "{key} is still recorded");
        }
    }

    #[test]
    fn switching_raw_storage_off_drops_the_bytes_and_nothing_else() {
        let device = advertised("Harbor Beacon");
        let identity = derive_device_identity(device.id.as_str());

        let kept = ble_signal_payload(&device, &identity, true);
        let dropped = ble_signal_payload(&device, &identity, false);

        assert!(
            !ble(&dropped).contains_key("raw_payload_hex"),
            "the setting has to actually withhold them: {dropped}"
        );

        let mut kept_without_bytes = kept.clone();
        kept_without_bytes
            .as_object_mut()
            .and_then(|outer| outer.get_mut("ble"))
            .and_then(|ble| ble.as_object_mut())
            .expect("the payload is a map of maps")
            .remove("raw_payload_hex");
        assert_eq!(kept_without_bytes, dropped);
    }

    #[test]
    fn a_backend_that_exposes_no_bytes_stores_no_key_rather_than_an_empty_one() {
        // btleplug and bluer report decoded properties, so `raw_payload` is None:
        // that has to read as "not available", never as "the device advertised
        // nothing", which would be indistinguishable from a real empty
        // advertisement in a query.
        let device = observed_device();
        assert!(device.raw_payload.is_none());

        let payload =
            ble_signal_payload(&device, &derive_device_identity(device.id.as_str()), true);

        assert!(!ble(&payload).contains_key("raw_payload_hex"), "{payload}");
    }

    #[test]
    fn an_empty_advertisement_is_a_payload_and_not_an_absence() {
        let device = observed_device().with_raw_payload(Vec::new());

        let payload =
            ble_signal_payload(&device, &derive_device_identity(device.id.as_str()), true);

        assert_eq!(ble(&payload)["raw_payload_hex"], serde_json::json!(""));
    }

    #[test]
    fn the_raw_bytes_change_what_the_node_attests_to() {
        let identity = test_identity();
        let device = advertised("Harbor Beacon");
        let device_identity = derive_device_identity(device.id.as_str());

        let with_bytes = build_from(
            &identity,
            &device,
            None,
            epoch(),
            epoch(),
            ble_signal_payload(&device, &device_identity, true),
        );
        let without = build_from(
            &identity,
            &device,
            None,
            epoch(),
            epoch(),
            ble_signal_payload(&device, &device_identity, false),
        );

        assert_ne!(with_bytes.signed_payload, without.signed_payload);
        assert_ne!(with_bytes.signature, without.signature);

        // The attestation carries the same bytes the row holds, so a reader with
        // the row and the node's key can check them rather than trust the column.
        assert_eq!(
            signed(&with_bytes).signed_signal_payload(),
            Some(
                canonical_signal_payload(&with_bytes.signal_payload)
                    .unwrap()
                    .as_slice()
            )
        );
        identity
            .verify(
                &with_bytes.signed_payload,
                &ed25519_dalek::Signature::try_from(with_bytes.signature.as_slice()).unwrap(),
            )
            .expect("the stored signature verifies over the stored payload");
    }

    #[test]
    fn a_stated_node_id_is_checked_against_the_key_that_has_to_back_it() {
        let identity = test_identity();

        assert!(assert_expected_identity(
            identity.node_id(),
            Some(identity.node_id()),
            Path::new("/var/lib/btmon")
        )
        .is_ok());
    }

    #[test]
    fn no_stated_node_id_leaves_the_key_file_in_charge() {
        let identity = test_identity();

        assert!(
            assert_expected_identity(identity.node_id(), None, Path::new("/var/lib/btmon")).is_ok()
        );
    }

    #[test]
    fn a_node_id_from_another_machine_names_both_ids_and_the_key_that_decided() {
        let identity = test_identity();
        let other = test_identity();
        let data_dir = tempfile::tempdir().unwrap();

        let error =
            assert_expected_identity(identity.node_id(), Some(other.node_id()), data_dir.path())
                .expect_err("two different keys cannot both be this node")
                .to_string();

        assert!(
            error.contains(&hex::encode(other.node_id())),
            "the id that was promised: {error}"
        );
        assert!(
            error.contains(&hex::encode(identity.node_id())),
            "the id this node actually has: {error}"
        );
        assert!(
            error.contains(NodeIdentity::IDENTITY_FILENAME),
            "the file that decided it: {error}"
        );
        assert!(error.contains("data_dir"), "how to move the node: {error}");
        assert!(
            error.contains("--node-id"),
            "how to restate the id: {error}"
        );
    }

    #[tokio::test]
    async fn startup_refuses_a_key_that_is_not_the_one_the_configuration_promised() {
        // The path an operator actually hits: a `NODE_ID` copied from another
        // device, or a data directory that moved. Nothing is signed and no row is
        // written, because the node never starts.
        let data_dir = tempfile::tempdir().unwrap();
        let other_node = test_identity();
        let config = FullNodeConfig::new(lazy_test_pool(), data_dir.path().to_path_buf())
            .with_expected_node_id(Some(other_node.node_id().to_vec()));

        let Err(error) = FullNode::new(config).await else {
            panic!("a node whose key is not the promised identity must not start");
        };

        let AppError::Config(message) = error else {
            panic!("expected a configuration error, got {error:?}");
        };
        assert!(
            message.contains(&hex::encode(other_node.node_id())),
            "{message}"
        );
    }

    #[tokio::test]
    async fn startup_proceeds_when_the_configuration_describes_the_key_it_loaded() {
        let data_dir = tempfile::tempdir().unwrap();
        let node_id = NodeIdentity::load_or_create(data_dir.path())
            .expect("a key in a fresh directory")
            .node_id()
            .to_vec();
        let config = FullNodeConfig::new(lazy_test_pool(), data_dir.path().to_path_buf())
            .with_expected_node_id(Some(node_id))
            .with_store_raw_payload(false);

        let node = FullNode::new(config)
            .await
            .expect("matching identity starts");

        assert_eq!(node.node_id().len(), 32);
        assert!(!node.store_raw_payload);
    }

    #[test]
    fn test_full_node_stats_clone() {
        let stats = FullNodeStats {
            total_events: 100,
            occurrences_stored: 80,
            occurrences_rate_limited: 20,
            storage_errors: 0,
            stream_reopens: 3,
            sampling: ObservationStats {
                present: 12,
                sightings: 100,
                recorded: 80,
                suppressed: 20,
                reobservations: 35,
                content_changes_held: 2,
                presence_expired: 1,
                reobservation_dropped: 0,
                removed: 4,
            },
            rate_limiter_stats: RateLimiterStats {
                cache_size: 50,
                allow_count: 80,
                deny_count: 20,
                cache_hit_rate: 20.0,
                threshold_ms: 15_000,
            },
        };

        let cloned = stats.clone();
        assert_eq!(cloned.total_events, 100);
        assert_eq!(cloned.occurrences_stored, 80);
        assert_eq!(cloned.occurrences_rate_limited, 20);
    }

    #[test]
    fn test_stats_display() {
        let stats = FullNodeStats {
            total_events: 100,
            occurrences_stored: 80,
            occurrences_rate_limited: 20,
            storage_errors: 1,
            stream_reopens: 0,
            sampling: ObservationStats {
                present: 12,
                sightings: 100,
                recorded: 80,
                suppressed: 20,
                reobservations: 35,
                content_changes_held: 2,
                presence_expired: 1,
                reobservation_dropped: 0,
                removed: 4,
            },
            rate_limiter_stats: RateLimiterStats {
                cache_size: 50,
                allow_count: 80,
                deny_count: 20,
                cache_hit_rate: 20.0,
                threshold_ms: 15_000,
            },
        };

        let _display = format!("{:?}", stats);
    }

    /// Fake monitor whose event stream yields a single event and then closes
    /// on every open, used to verify that [`FullNode::run`] supervises the
    /// stream and reopens it instead of exiting.
    struct ClosingMonitor {
        event_open_times: Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
    }

    #[async_trait::async_trait]
    impl DeviceMonitor for ClosingMonitor {
        async fn start_scan(&self) -> bt_mon::Result<()> {
            Ok(())
        }

        async fn stop_scan(&self) -> bt_mon::Result<()> {
            Ok(())
        }

        async fn devices(&self) -> bt_mon::Result<Vec<BluetoothDevice>> {
            Ok(Vec::new())
        }

        async fn device(&self, id: &bt_mon::DeviceId) -> bt_mon::Result<BluetoothDevice> {
            Err(bt_mon::Error::DeviceNotFound(id.clone()))
        }

        async fn is_powered(&self) -> bt_mon::Result<bool> {
            Ok(true)
        }

        async fn adapter_info(&self) -> bt_mon::Result<String> {
            Ok("closing monitor".to_string())
        }

        async fn device_events(
            &self,
        ) -> bt_mon::Result<bt_mon::monitor::events::DeviceEventStream> {
            self.event_open_times
                .lock()
                .unwrap()
                .push(std::time::Instant::now());

            let device = BluetoothDevice::new(
                bt_mon::DeviceId::new("AA:BB:CC:DD:EE:FF"),
                "AA:BB:CC:DD:EE:FF".to_string(),
            );
            Ok(Box::pin(futures_util::stream::iter(std::iter::once(
                DeviceEvent::DeviceAdded { device },
            ))))
        }

        async fn is_scanning(&self) -> bt_mon::Result<bool> {
            Ok(true)
        }
    }

    /// DB pool that never actually connects: the test only exercises event
    /// stream supervision, and storage failures are expected and counted.
    fn lazy_test_pool() -> Pool {
        let options: sqlx::postgres::PgConnectOptions =
            std::str::FromStr::from_str("postgres://test:test@127.0.0.1:9/test")
                .expect("valid connection string");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(1))
            .connect_lazy_with(options);
        Pool::from_pool(pool)
    }

    #[tokio::test]
    async fn test_run_reopens_closed_event_stream() {
        let data_dir = tempfile::tempdir().unwrap();
        let config = FullNodeConfig::new(lazy_test_pool(), data_dir.path().to_path_buf())
            .with_stream_reopen_delay(250);
        let node = Arc::new(FullNode::new(config).await.unwrap());

        let open_times: Arc<std::sync::Mutex<Vec<std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut monitor: Box<dyn DeviceMonitor + Send + Sync> = Box::new(ClosingMonitor {
            event_open_times: open_times.clone(),
        });

        let node_task = node.clone();
        let handle = tokio::spawn(async move {
            let _ = node_task.run(&mut *monitor).await;
        });

        // Each open yields exactly one event and then the stream closes, so
        // two reopens imply three opens and three consumed events.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let stats = node.stats();
            if stats.stream_reopens >= 2 && stats.total_events >= 3 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for stream reopens: {:?}",
                stats
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        handle.abort();

        // Consecutive stream opens must be spaced by at least the configured
        // reopen delay (with slack for timer resolution).
        let times = open_times.lock().unwrap().clone();
        assert!(
            times.len() >= 3,
            "expected at least 3 stream opens, got {}",
            times.len()
        );
        for window in times.windows(2) {
            let gap = window[1].duration_since(window[0]);
            assert!(
                gap >= Duration::from_millis(200),
                "stream reopened after {:?}, expected >= ~250ms delay",
                gap
            );
        }
    }

    // --------------------------------------------------------------------------------
    // Sampling: what a node writes for a device it can see (GAP_ANALYSIS B14).
    // --------------------------------------------------------------------------------

    #[cfg(feature = "mock")]
    use bt_mon::backends::mock::MockMonitor;
    #[cfg(feature = "mock")]
    use repo::models::NodeType;

    /// A timestamp this schema will accept a row for.
    ///
    /// `occurrences` is partitioned on `observed_at` and `ensure_occurrence_partition`
    /// refuses to create a partition outside a rolling window around now, so the
    /// shared position-test epoch — a perfectly good instant from 2023 — is not a
    /// usable one for anything that stores.
    #[cfg(feature = "mock")]
    fn sampling_epoch() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
    }

    /// A node writing to the test database, on a clock the test moves, with the
    /// simulated radio it re-observes from.
    ///
    /// The clock matters as much as the database here: a window is fifteen seconds,
    /// and a test that waited for that would be slow, flaky, and still only proving
    /// something about how fast the machine running it is.
    #[cfg(feature = "mock")]
    async fn sampling_node(
        pool: sqlx::PgPool,
        window_ms: u64,
    ) -> (FullNode, Arc<ManualClock>, MockMonitor, tempfile::TempDir) {
        let clock = Arc::new(ManualClock::new(sampling_epoch()));
        let data_dir = tempfile::tempdir().expect("a data directory");
        let config =
            FullNodeConfig::new(Pool::from_pool(pool.clone()), data_dir.path().to_path_buf())
                .with_rate_limit_threshold(window_ms)
                .with_clock(clock.clone());
        let node = FullNode::new(config).await.expect("a node");

        // `occurrences.origin_node_id` references `nodes(node_id)`, so nothing can be
        // stored until this node is enrolled.
        NodeRepository::register(
            &pool,
            node.node_id(),
            NodeType::Full,
            node.identity.verifying_key().as_bytes(),
            b"ca-credential",
            None,
            &[],
        )
        .await
        .expect("the node should register");

        (node, clock, MockMonitor::new(), data_dir)
    }

    /// Every `observed_at` written so far, oldest first — the row count and its
    /// spacing in one read.
    #[cfg(feature = "mock")]
    async fn observed_times(pool: &sqlx::PgPool) -> Vec<DateTime<Utc>> {
        sqlx::query_scalar("SELECT observed_at FROM occurrences ORDER BY observed_at")
            .fetch_all(pool)
            .await
            .expect("the rows written so far")
    }

    #[cfg(feature = "mock")]
    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_stationary_beacon_yields_one_occurrence_per_window(pool: sqlx::PgPool) {
        // The exit criterion, end to end: a beacon that never moves and never has
        // anything new to say. It used to produce exactly one occurrence, because
        // the node stored on `DeviceAdded` and on an RSSI change, and a device at
        // rest supplies neither after the first moment.
        let (node, clock, _monitor, _dir) = sampling_node(pool.clone(), 15_000).await;
        let beacon = beacon("iBeacon", -59);

        // Sixty seconds of reports, ten a second.
        for _ in 0..600 {
            node.observe(&beacon).await;
            clock.advance_ms(100);
        }

        let rows = observed_times(&pool).await;
        assert_eq!(
            rows.len(),
            4,
            "one at discovery plus one per elapsed window in fifty-nine seconds"
        );
        for pair in rows.windows(2) {
            assert_eq!(
                pair[1] - pair[0],
                chrono::Duration::seconds(15),
                "the rows are one window apart, not one per report"
            );
        }
    }

    #[cfg(feature = "mock")]
    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn signal_jitter_no_longer_drives_the_record_rate(pool: sqlx::PgPool) {
        // The same sixty seconds with a device held at arm's length: a different
        // RSSI in every report. Those used to be the only reports the node stored,
        // so this device used to be the busy one. It now costs the same four rows as
        // the beacon that never moved.
        let (node, clock, _monitor, _dir) = sampling_node(pool.clone(), 15_000).await;

        for tick in 0..600 {
            node.observe(&beacon("Phone", -40 - (tick % 20))).await;
            clock.advance_ms(100);
        }

        assert_eq!(observed_times(&pool).await.len(), 4);
    }

    #[cfg(feature = "mock")]
    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_beacon_nobody_reports_is_still_observed_every_window(pool: sqlx::PgPool) {
        // The lower bound, which is the half no backend can be trusted to provide:
        // a radio that says nothing further about a device that is still there.
        let (node, clock, monitor, _dir) = sampling_node(pool.clone(), 15_000).await;
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        monitor
            .add_device(id.clone())
            .await
            .expect("simulated device");
        let device = monitor.device(&id).await.expect("the simulated device");

        node.observe(&device).await;
        assert_eq!(observed_times(&pool).await.len(), 1, "discovery");

        for window in 1..=3 {
            clock.advance_ms(15_000);
            node.sample_present_devices(&monitor).await;

            let rows = observed_times(&pool).await;
            assert_eq!(rows.len(), window + 1, "window {window} is owed a row");
            assert_eq!(
                rows[rows.len() - 1] - rows[rows.len() - 2],
                chrono::Duration::seconds(15),
                "each re-observation is stamped when it was read, not when the \
                 device was first seen"
            );
        }
        assert_eq!(node.stats().sampling.reobservations, 3);

        // Longer than presence is trusted with nobody reporting it, and the node
        // stops. It has no reading, and a row would claim one.
        clock.advance_ms(60_000);
        node.sample_present_devices(&monitor).await;
        assert_eq!(
            observed_times(&pool).await.len(),
            4,
            "a device nobody reported is not a device observed"
        );
        assert_eq!(node.stats().sampling.presence_expired, 1);
    }

    #[cfg(feature = "mock")]
    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_device_the_monitor_lost_is_never_written_from_memory(pool: sqlx::PgPool) {
        // A cached snapshot and a fresh timestamp is not an observation, it is a
        // fabrication, so the re-observation path writes nothing at all when the
        // monitor has nothing to give.
        let (node, clock, monitor, _dir) = sampling_node(pool.clone(), 15_000).await;
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        monitor
            .add_device(id.clone())
            .await
            .expect("simulated device");
        node.observe(&monitor.device(&id).await.expect("the device"))
            .await;
        assert_eq!(observed_times(&pool).await.len(), 1);

        // The adapter forgets the device without anyone reporting it absent.
        assert!(monitor.remove_device(&id).await, "the device was there");
        for _ in 0..4 {
            clock.advance_ms(15_000);
            node.sample_present_devices(&monitor).await;
        }

        assert_eq!(observed_times(&pool).await.len(), 1, "no reading, no row");
        let sampling = node.stats().sampling;
        assert_eq!(
            sampling.reobservation_dropped, 1,
            "dropped once, not per tick"
        );
        assert_eq!(sampling.reobservations, 0);
    }

    #[cfg(feature = "mock")]
    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_device_reported_absent_stops_being_sampled(pool: sqlx::PgPool) {
        let (node, clock, monitor, _dir) = sampling_node(pool.clone(), 15_000).await;
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        monitor
            .add_device(id.clone())
            .await
            .expect("simulated device");
        let device = monitor.device(&id).await.expect("the simulated device");
        node.observe(&device).await;

        // The backend reports it gone: the window it was inside is over.
        node.handle_absence(&id);
        clock.advance_ms(1_000);

        // Nothing is owed for a device nobody reports, whether it is trusted present
        // or not.
        node.sample_present_devices(&monitor).await;
        assert_eq!(observed_times(&pool).await.len(), 1);

        // Coming back one second after it was recorded is a new co-presence, and is
        // recorded — inside what would still have been the old window had the absence
        // not closed it.
        monitor.add_device(id.clone()).await.expect("device back");
        node.observe(&monitor.device(&id).await.expect("the device"))
            .await;
        let rows = observed_times(&pool).await;
        assert_eq!(rows.len(), 2, "a return is worth a row");
        assert_eq!(
            rows[1] - rows[0],
            chrono::Duration::seconds(1),
            "one second after the first record, which the window would otherwise \
             have suppressed"
        );

        // And a second report a second after that is suppressed again: dropping the
        // window on absence is not the same as dropping the window.
        clock.advance_ms(1_000);
        node.observe(&monitor.device(&id).await.expect("the device"))
            .await;
        assert_eq!(observed_times(&pool).await.len(), 2);
    }

    #[cfg(feature = "mock")]
    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_content_change_waits_for_the_window_like_everything_else(pool: sqlx::PgPool) {
        // A beacon that rewrites its payload every second is the device the window
        // exists to bound. The change is not lost — the next record says it was a
        // change — but it does not buy a row on its own.
        let (node, clock, _monitor, _dir) = sampling_node(pool.clone(), 15_000).await;
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");

        for second in 0..30 {
            let mut device = BluetoothDevice::new(id.clone(), id.as_str().to_string())
                .with_name("Sensor")
                .with_rssi(-60);
            device.manufacturer_data.insert(0x004C, vec![second as u8]);
            node.observe(&device).await;
            clock.advance_ms(1_000);
        }

        let rows = observed_times(&pool).await;
        assert_eq!(
            rows.len(),
            2,
            "thirty different payloads over thirty seconds still cost one row per window: \
             discovery and the window that closed at fifteen seconds"
        );

        // Each row carries the payload current when it was written, not the one the
        // device was first seen with — the change is held, not discarded.
        let payloads: Vec<String> = sqlx::query_scalar(
            "SELECT signal_payload->'ble'->'manufacturer_data'->>'payload' \
             FROM occurrences ORDER BY observed_at",
        )
        .fetch_all(&pool)
        .await
        .expect("the payloads written");
        assert_eq!(
            payloads,
            vec!["00".to_string(), "0f".to_string()],
            "the second row is the report from second 15, when the window reopened"
        );
    }

    /// Rows in `occurrences`, however they got there.
    async fn occurrence_count(pool: &sqlx::PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM occurrences")
            .fetch_one(pool)
            .await
            .expect("a row count")
    }

    /// A node with `[revocation]` switched on, against a CA that publishes to this
    /// database and whose anchor file the node was pointed at.
    ///
    /// `build_list` is handed the fresh CA and the node's own node id, because the
    /// cases worth distinguishing are "this node is on the list" and "this node is
    /// not", and that id is only known once the identity exists in the data
    /// directory the node will load.
    ///
    /// Returns `Err` when the node refused to start, which is itself behaviour under
    /// test.
    async fn revocation_node(
        pool: sqlx::PgPool,
        max_staleness_secs: u64,
        build_list: impl FnOnce(&ca::CaRoot, &[u8]) -> ca::RevocationStatusList,
    ) -> std::result::Result<(FullNode, ca::CaRoot, tempfile::TempDir), AppError> {
        use ca::RslManager;

        let dir = tempfile::tempdir().expect("a data directory");
        let identity = NodeIdentity::load_or_create(dir.path()).expect("an identity to revoke");

        let ca = ca::CaRoot::generate();
        ca::DatabaseRslManager::new(pool.clone())
            .store_rsl(&build_list(&ca, identity.node_id()))
            .await
            .expect("publish the list");

        let anchor_path = dir.path().join("anchor.pem");
        ca.trust_anchor()
            .save_to_file(&anchor_path)
            .expect("write the anchor");

        let config = FullNodeConfig {
            revocation: RevocationConfig {
                enabled: true,
                anchor_path: Some(anchor_path.display().to_string()),
                max_staleness_secs,
                refresh_secs: 15 * 60,
            },
            ..FullNodeConfig::new(Pool::from_pool(pool.clone()), dir.path().to_path_buf())
        };

        let node = FullNode::new(config).await?;

        // `occurrences.origin_node_id` references `nodes(node_id)`, so without this
        // row a test that saw nothing stored could not tell the revocation gate's
        // refusal from a foreign key's.
        NodeRepository::register(
            &pool,
            node.node_id(),
            repo::models::NodeType::Full,
            node.identity.verifying_key().as_bytes(),
            b"ca-credential",
            None,
            &[],
        )
        .await
        .expect("the node should register");

        Ok((node, ca, dir))
    }

    /// A node with no CA configured, for the checks that must not depend on one.
    async fn plain_node(pool: sqlx::PgPool) -> (FullNode, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a data directory");
        let config = FullNodeConfig::new(Pool::from_pool(pool), dir.path().to_path_buf());
        let node = FullNode::new(config).await.expect("a node");
        (node, dir)
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_revoked_node_records_nothing(pool: sqlx::PgPool) {
        // B11's exit criterion as an operator would state it: a node whose key its CA
        // has revoked stores nothing — even though its radio keeps hearing devices
        // and its own sampling policy says to record them.
        let (node, _ca, _dir) = revocation_node(pool.clone(), 60 * 60, |ca, node_id| {
            crate::node::revocation::list_revoking(ca, node_id, 1, 0, 7)
        })
        .await
        .expect("a current list, so the node starts");

        node.observe(&observed_device()).await;

        assert_eq!(occurrence_count(&pool).await, 0, "nothing stored");
        let stats = node.stats();
        assert_eq!(stats.occurrences_stored, 0);
        assert_eq!(
            stats.storage_errors, 1,
            "the refusal is reported rather than swallowed: a node that quietly stores nothing \
             looks exactly like one that heard nothing"
        );
        assert_eq!(
            stats.occurrences_rate_limited, 0,
            "the observation reached the store path rather than being suppressed by a window"
        );
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_node_its_ca_has_not_revoked_records_as_usual(pool: sqlx::PgPool) {
        // The control. Without it the test above proves only that this harness stores
        // nothing: the list here is current, verifies under the anchor, and simply
        // does not name this node.
        let (node, _ca, _dir) = revocation_node(pool.clone(), 60 * 60, |ca, _node_id| {
            crate::node::revocation::list_revoking(ca, &[0x77u8; 32], 1, 0, 7)
        })
        .await
        .expect("a current list naming somebody else");

        node.observe(&observed_device()).await;

        assert_eq!(occurrence_count(&pool).await, 1, "recorded as ever");
        assert_eq!(node.stats().storage_errors, 0);
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_list_past_its_staleness_bound_stops_the_node_starting(pool: sqlx::PgPool) {
        // Issued three days ago and still inside its validity window, so it verifies:
        // the staleness bound is the only thing between this node and reading a
        // three-day-old silence as an all-clear. Running with that would mean storing
        // occurrences whose reporting node the CA may have revoked yesterday, so the
        // node declines to start instead.
        let err = match revocation_node(pool.clone(), 60 * 60, |ca, _node_id| {
            crate::node::revocation::list_revoking(ca, &[0x77u8; 32], 1, 3, 7)
        })
        .await
        {
            Ok((_node, _ca, _dir)) => {
                panic!("a node that cannot trust its list must not start and store anyway")
            }
            Err(e) => e,
        };

        let message = err.to_string();
        assert!(
            message.contains("revocation checking is enabled but unusable"),
            "{message}"
        );
        assert!(
            message.contains("ca-generate-rsl"),
            "the error names the fix, not just the failure: {message}"
        );
        assert_eq!(occurrence_count(&pool).await, 0);
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_peer_is_verified_against_its_own_key(pool: sqlx::PgPool) {
        // GAP_ANALYSIS M17. Verification used to run against *this* node's key, which
        // means a peer could never pass and anyone holding this node's key could. Both
        // halves are pinned, because "it rejects things" is not the property —
        // rejecting the wrong things is the bug.
        let (node, _dir) = plain_node(pool.clone()).await;

        let peer_dir = tempfile::tempdir().expect("a peer data directory");
        let peer = NodeIdentity::load_or_create(peer_dir.path()).expect("a peer identity");
        NodeRepository::register(
            &pool,
            peer.node_id(),
            repo::models::NodeType::Full,
            peer.verifying_key().as_bytes(),
            b"ca-credential",
            None,
            &[],
        )
        .await
        .expect("register the peer");

        let payload = b"an occurrence attributed to the peer";

        let theirs = peer.sign(payload);
        node.verify_received_occurrence(peer.node_id(), payload, &theirs.to_bytes())
            .await
            .expect("a peer signing with its own key verifies");

        let ours = node.identity.sign(payload);
        let err = node
            .verify_received_occurrence(peer.node_id(), payload, &ours.to_bytes())
            .await
            .expect_err("this node's own key must not vouch for a peer");
        assert!(
            err.to_string()
                .contains("does not verify under its registered key"),
            "{err}"
        );

        // And the peer's key does not make any bytes at all valid.
        let elsewhere = peer.sign(b"a different occurrence entirely");
        assert!(node
            .verify_received_occurrence(peer.node_id(), payload, &elsewhere.to_bytes())
            .await
            .is_err());
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_peer_that_is_not_enrolled_has_no_key_to_verify(pool: sqlx::PgPool) {
        // A signature is verified against the registry's key or not at all, so an
        // unknown node id has no key and is refused before anyone asks whether the
        // bytes are well formed.
        let (node, _dir) = plain_node(pool.clone()).await;

        let err = node
            .verify_received_occurrence(&[0x5au8; 32], b"anything at all", &[0u8; 64])
            .await
            .expect_err("no registry row, no key");
        assert!(err.to_string().contains("not enrolled"), "{err}");
    }

    #[sqlx::test(migrator = "db::SQLX_FORWARD_MIGRATOR")]
    async fn a_registry_row_whose_key_is_not_its_node_id_verifies_nothing(pool: sqlx::PgPool) {
        // The registry is trusted, but not blindly: a node id *is* SHA-256 of its
        // signing key, so a row that breaks that is either corrupt or planted, and a
        // signature checked under its key would prove possession of some key while
        // being reported as proof about the id the occurrence carried.
        let (node, _dir) = plain_node(pool.clone()).await;

        let honest =
            NodeIdentity::load_or_create(tempfile::tempdir().expect("a data directory").path())
                .expect("an identity to register inconsistently");
        let someone_else =
            NodeIdentity::load_or_create(tempfile::tempdir().expect("a data directory").path())
                .expect("a second identity");

        // The id of one node, the key of another.
        NodeRepository::register(
            &pool,
            honest.node_id(),
            repo::models::NodeType::Full,
            someone_else.verifying_key().as_bytes(),
            b"ca-credential",
            None,
            &[],
        )
        .await
        .expect("register the mismatched row");

        // Signed by the key the row holds, so the signature itself is fine — which is
        // exactly why the id has to be checked before it is believed.
        let payload = b"an occurrence attributed to the first node";
        let signature = someone_else.sign(payload);

        let err = node
            .verify_received_occurrence(honest.node_id(), payload, &signature.to_bytes())
            .await
            .expect_err("a key that is not the node it claims");
        assert!(
            err.to_string().contains("is not that node's key"),
            "the message should say which of the two does not match: {err}"
        );
    }
}
