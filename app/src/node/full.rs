//! FullNode implementation for Phase 0.
//!
//! This module provides the complete Phase 0 node implementation that:
//! 1. Monitors Bluetooth devices
//! 2. Signs occurrences with node identity
//! 3. Rate-limits storage to avoid duplicates
//! 4. Stores signed occurrences in the database
//! 5. Checks revocation status for received occurrences (P2P mode)
//!
//! # Revocation Checking
//!
//! The FullNode supports revocation checking for verifying occurrences from other nodes:
//!
//! 1. **Data Recording** (lenient policy): Accept occurrences from unknown nodes with warning
//!    - Use `verify_received_occurrence()` when storing occurrences received via P2P
//!
//! 2. **Peer Connections** (strict policy): Reject connections from unknown/revoked nodes
//!    - Use `should_allow_peer_connection()` during P2P handshake
//!
//! To enable revocation checking, set `enable_revocation_checking: true` in `FullNodeConfig`.
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

use bt_mon::monitor::events::UpdateField;
use bt_mon::{BluetoothDevice, DeviceEvent, DeviceMonitor};
use chrono::{DateTime, Utc};
use futures_util::stream::StreamExt;
use log::{debug, error, info, warn};
use repo::models::LocationSource;
use repo::{NodeRepository, Occurrence, OccurrenceRepository, Pool, SignalType};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::node::identity::NodeIdentity;
use crate::node::{
    derive_device_identity, Clock, DeviceIdentity, Node, RateLimiter, RateLimiterConfig,
    RateLimiterStats, SystemClock,
};
use crate::position::{BestEffortPositionSource, NoPositionSource, Position, PositionSource};
use crate::provenance::encode::encode_payload;
use crate::provenance::payload::{truncate_to_micros, PayloadV2, CURRENT_VERSION};
use ca::{InMemoryRslChecker, RevocationChecker};

/// CoreBluetooth (and any other backend that hides the MAC address) yields
/// host-local identifiers, which changes what `device_hash` means. Worth saying
/// out loud exactly once rather than on every scan event.
static NON_MAC_IDENTIFIER_WARNED: Once = Once::new();

/// `store_raw_payload` promises the radio's bytes; a backend that cannot supply
/// them has to say so out loud, once, instead of storing a payload that quietly
/// lacks the key the operator turned on.
static RAW_PAYLOAD_ABSENT_WARNED: Once = Once::new();

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

    /// Use mock backend for testing/development (no physical Bluetooth required).
    #[cfg(feature = "mock")]
    pub use_mock_backend: bool,

    /// Enable revocation checking (default: false).
    /// When true, occurrences from revoked nodes will be rejected.
    pub enable_revocation_checking: bool,
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
            #[cfg(feature = "mock")]
            use_mock_backend: false,
            enable_revocation_checking: false, // Disabled by default for backwards compatibility
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

    /// Enable or disable revocation checking.
    pub fn with_revocation_checking(mut self, enabled: bool) -> Self {
        self.enable_revocation_checking = enabled;
        self
    }

    /// Store (or drop) the radio's advertisement bytes in `signal_payload`.
    pub fn with_store_raw_payload(mut self, enabled: bool) -> Self {
        self.store_raw_payload = enabled;
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

    /// Number of occurrences rate-limited.
    pub occurrences_rate_limited: usize,

    /// Number of storage errors.
    pub storage_errors: usize,

    /// Number of times the device event stream was (re)opened after closing.
    pub stream_reopens: usize,

    /// Rate limiter statistics.
    pub rate_limiter_stats: RateLimiterStats,
}

/// The Phase 0 FullNode implementation.
///
/// A FullNode is a standalone node that:
/// - Monitors Bluetooth devices on its local adapter
/// - Signs all occurrences with its node identity
/// - Rate-limits storage to avoid duplicates
/// - Stores signed occurrences in its local database
/// - Checks revocation status before storing occurrences
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

    /// Rate limiter for deduplication.
    rate_limiter: Arc<RateLimiter>,

    /// Clock for timestamps.
    clock: Arc<dyn Clock>,

    /// Where the node is, acquired per occurrence.
    position: BestEffortPositionSource,

    /// Revocation checker for verifying node authenticity.
    revocation_checker: Arc<InMemoryRslChecker>,

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

        // Create rate limiter
        let rate_limiter_config = RateLimiterConfig::with_threshold(Duration::from_millis(
            config.rate_limit_threshold_ms,
        ))
        .with_max_cache_size(config.rate_limit_max_cache_size.unwrap_or(usize::MAX));
        let rate_limiter = Arc::new(RateLimiter::with_config(rate_limiter_config));

        // Revocation checker with no list loaded. It answers Unknown for every
        // node until an RSL is loaded into it — which nothing does yet, because
        // there is no trust anchor to verify an RSL against (GAP_ANALYSIS B11,
        // B12) — so `Unknown` here means "the CA has not been asked", never
        // "this node is valid".
        let revocation_checker = Arc::new(InMemoryRslChecker::new(chrono::Duration::hours(24)));
        if config.enable_revocation_checking {
            info!("Revocation checking enabled");
        } else {
            info!("Revocation checking disabled");
        }

        Ok(Self {
            identity,
            pool: config.pool,
            rate_limiter,
            clock: Arc::new(SystemClock),
            position: BestEffortPositionSource::new(config.position),
            revocation_checker,
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
                 app ca ca-enroll --public-key {}",
                hex::encode(self.identity.node_id()),
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
        info!(
            "Listening for Bluetooth events (press Ctrl+C to stop; a closed event stream will be reopened after {:?})",
            reopen_delay
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

            // Consume events until the stream closes.
            while let Some(event) = events.next().await {
                self.total_events
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                match event {
                    DeviceEvent::DeviceAdded { device } => {
                        info!("Discovered device: {}", device.id);
                        if let Err(e) = self.store_occurrence(&device).await {
                            error!("Error storing occurrence: {}", e);
                            self.storage_errors
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }

                    DeviceEvent::DeviceRemoved { id } => {
                        debug!("Device removed: {}", id);
                    }

                    DeviceEvent::DeviceUpdated {
                        device,
                        changed_fields,
                    } => {
                        debug!(
                            "Device updated: {} (changed: {:?})",
                            device.id, changed_fields
                        );

                        // Handle RSSI updates - store as new occurrence
                        if changed_fields.contains(&UpdateField::Rssi) {
                            if let Err(e) = self.store_occurrence(&device).await {
                                error!("Error storing occurrence: {}", e);
                                self.storage_errors
                                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
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

    /// Store an occurrence for a Bluetooth device.
    ///
    /// This method:
    /// 1. Normalizes the backend device identifier into an address and device hash
    /// 2. Checks the rate limiter (skips if limited)
    /// 3. Checks revocation status (if enabled)
    /// 4. Builds a canonical payload
    /// 5. CBOR encodes the payload
    /// 6. Signs the encoded bytes
    /// 7. Inserts the occurrence into the database
    ///
    /// Backends that do not expose a MAC address (CoreBluetooth reports a
    /// host-local UUID) store a NULL `device_address`; the identifier is still
    /// covered by `device_hash`, and `signal_payload` records how it was derived.
    ///
    /// # Arguments
    ///
    /// * `device` - The Bluetooth device to store
    ///
    /// # Returns
    ///
    /// * `Ok(())` - The occurrence was stored (or rate-limited)
    /// * `Err(AppError)` - If storage failed
    async fn store_occurrence(&self, device: &BluetoothDevice) -> Result<()> {
        // Normalize the backend identifier: MAC on BlueZ, host-local UUID on
        // CoreBluetooth. Infallible, so an unrecognized form never drops events.
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

        // Check rate limiter (keyed on device_hash, which exists for every
        // identifier source unlike the optional address)
        if !self.rate_limiter.should_store(&identity.hash) {
            debug!("Rate limited: {}", device.id);
            self.occurrences_rate_limited
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }

        // Note: Revocation checking for received occurrences from other nodes
        // should be done via verify_received_occurrence() before calling this method.
        // This method only stores locally-generated occurrences.

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
            rate_limiter_stats: self.rate_limiter.stats(),
        }
    }

    /// Get the rate limit threshold in milliseconds.
    pub fn rate_limit_threshold_ms(&self) -> u64 {
        self.rate_limiter.stats().threshold_ms
    }

    /// Clear the rate limiter cache.
    ///
    /// This is primarily useful for testing.
    pub fn clear_rate_limiter(&self) {
        self.rate_limiter.clear();
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

    /// Verify a received occurrence from another node, including revocation check.
    ///
    /// This is used when receiving occurrences via P2P from other nodes.
    /// It verifies:
    /// 1. The signature is valid
    /// 2. The origin node is not revoked
    ///
    /// # Arguments
    ///
    /// * `origin_node_id` - The ID of the node that created the occurrence
    /// * `_signing_public_key` - The node's signing public key (currently unused, signature verification uses embedded key)
    /// * `signed_payload` - The bytes that were signed
    /// * `signature` - The signature to verify
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Signature is valid and node is not revoked
    /// * `Err(AppError)` - If verification or revocation check failed
    pub async fn verify_received_occurrence(
        &self,
        origin_node_id: &[u8],
        _signing_public_key: &[u8],
        signed_payload: &[u8],
        signature: &[u8],
    ) -> Result<()> {
        // Verify the signature first
        self.identity
            .verify(
                signed_payload,
                &ed25519_dalek::Signature::try_from(signature).map_err(|e| {
                    AppError::Validation(format!("Invalid signature format: {}", e))
                })?,
            )
            .map_err(|e| AppError::Validation(format!("Signature verification failed: {}", e)))?;

        // Check revocation status (if enabled)
        let status = self
            .revocation_checker
            .is_revoked(origin_node_id)
            .map_err(|e| AppError::Validation(format!("Revocation check failed: {}", e)))?;

        match status {
            ca::RevocationStatus::Valid => {
                debug!(
                    "Node {} is valid for data recording",
                    hex::encode(origin_node_id)
                );
                Ok(())
            }
            ca::RevocationStatus::Revoked => {
                warn!(
                    "Node {} has been revoked - rejecting occurrence",
                    hex::encode(origin_node_id)
                );
                Err(AppError::Provenance(format!(
                    "Node {} has been revoked",
                    hex::encode(origin_node_id)
                )))
            }
            ca::RevocationStatus::Unknown => {
                // DataRecordingPolicy accepts unknown with warning
                warn!(
                    "Node {} revocation status unknown - accepting with warning",
                    hex::encode(origin_node_id)
                );
                Ok(())
            }
        }
    }

    /// Check if a peer node should be allowed to connect (P2P handshake).
    ///
    /// This uses a stricter policy than data recording - it rejects nodes
    /// with revoked or unknown status.
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
        let status = self
            .revocation_checker
            .is_revoked(peer_node_id)
            .map_err(|e| AppError::Validation(format!("Revocation check failed: {}", e)))?;

        match status {
            ca::RevocationStatus::Valid => Ok(true),
            ca::RevocationStatus::Revoked => {
                warn!(
                    "Rejecting connection from revoked node {}",
                    hex::encode(peer_node_id)
                );
                Ok(false)
            }
            ca::RevocationStatus::Unknown => {
                // ConnectionPolicy rejects unknown
                warn!(
                    "Rejecting connection from node with unknown status {}",
                    hex::encode(peer_node_id)
                );
                Ok(false)
            }
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
    use crate::position::test_support::epoch;
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
}
