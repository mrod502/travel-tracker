//! When the node records an occurrence for a device it can see.
//!
//! # The problem this decides
//!
//! A Bluetooth radio does not report a device once and then only when it changes.
//! With duplicate reporting enabled — which `bt_mon` asks BlueZ for — every
//! advertisement produces a report, and a device that stays in range keeps
//! producing them for as long as it is there. So the node, not the radio, owns the
//! question **how often do I write a row for this device?** Before this module the
//! answer was an accident of which events the node happened to notice: store on
//! `DeviceAdded`, store on an RSSI change, drop everything else.
//!
//! The observable consequence ([GAP_ANALYSIS B14](../../GAP_ANALYSIS.md)) was a
//! stationary beacon producing exactly one occurrence. A device that does not move
//! has a stable signal, so the one field the node watched never moved, so the node
//! recorded it on discovery and then never again — while a device held at arm's
//! length by a hand that shakes produced a row per RSSI tick. Occurrence volume
//! was a function of signal jitter. For a co-presence network that is inverted:
//! the data is thinnest exactly where the signal is steadiest.
//!
//! # The policy
//!
//! Sampling, not change detection. Two bounds, both stated per device:
//!
//! * **At most one occurrence per [`window`](ObservationPolicy::window).** The
//!   sighting rate of the radio and the write rate of the node are decoupled. This
//!   is the bound `rate_limit_ms` was always meant to express and could not,
//!   because nothing was delivering an advertisement-rate stream to throttle.
//! * **At least one occurrence per `window` while the device is present.**
//!   [`ObservationPolicy::due`] names the devices whose window has elapsed; the
//!   node re-reads each from the monitor and records a fresh observation. A
//!   stationary beacon therefore yields one row per window instead of one row per
//!   visit.
//!
//! Both bounds are per device, so a dense room costs one row per window per device
//! and nothing more.
//!
//! # Where the window lives
//!
//! In [`RateLimiter`](crate::node::rate_limiter::RateLimiter), and nowhere else.
//! The window is an interval per device with one timestamp, and a policy that
//! tracked a second one would eventually disagree with the first about which
//! device is owed a row. So this module holds what the rate limiter does not have
//! and cannot: whether a device is *present*, and what it was advertising the last
//! time a row was written. Every timing question here is asked of the limiter.
//!
//! # What is deliberately *not* recorded
//!
//! A re-observation is a *new reading*, not a replay. The node re-reads the device
//! from the monitor rather than rewriting its cached snapshot with a fresh
//! timestamp, because a row's `observed_at` claims when the signal was observed:
//! stamping an hour-old RSSI with the current time would manufacture an
//! observation that never happened. So when the monitor no longer has a device the
//! node believed present, the node drops it and counts it
//! ([`ObservationStats::reobservation_dropped`]) — silence is recorded as silence.
//! The same reasoning keeps
//! [`presence_timeout`](ObservationPolicy::presence_timeout) finite: a device
//! nobody has reported for longer than it is trusted to remain is no longer
//! present, however convenient it would be to keep sampling it.
//!
//! The trade-off that follows is worth stating: a device that leaves range without
//! its backend noticing stops being reported, and the node keeps sampling it —
//! from a reading no older than the last report — until presence expires. Rows
//! written in that interval describe a device that may already have gone. The
//! alternative is to trust only the fields that change, which is the defect this
//! module replaces; a bounded, declared staleness is the cheaper error.
//!
//! Content changes are bounded the way everything else is. A changed
//! advertisement is real news and is never lost — [`sighting`](Self::sighting)
//! remembers it and the next record carries it
//! ([`RecordReason::ContentChanged`]) — but it does not buy an extra row inside a
//! window. A beacon that rewrites its payload every 10 ms is precisely the device
//! the window exists to bound.

use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::node::rate_limiter::RateLimiterStats;
use crate::node::{Clock, RateLimiter, RateLimiterConfig};

/// A device's advertisement contents, reduced to something comparable.
///
/// Deliberately excludes RSSI: a signal strength that wobbles by a dBm is not the
/// device having *changed*, and treating it as such is what made the node's record
/// rate track jitter rather than advertisements. The connection state is excluded
/// too — that describes the node's own GATT usage, not what is on the air.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AdvertisementContent {
    /// The digest of what the device advertised.
    digest: [u8; 32],
    /// What went into `digest`, in words, for logs.
    summary: String,
}

impl AdvertisementContent {
    /// Digest what a device advertised.
    ///
    /// `name`, `manufacturer_data` (company id → bytes), `service_data` (uuid →
    /// bytes) and the backend's undecoded `raw_payload` where it has one. The maps
    /// are ordered before hashing, so the digest depends on the contents and not
    /// on iteration order.
    pub fn new(
        name: Option<&str>,
        manufacturer_data: &BTreeMap<u16, Vec<u8>>,
        service_data: &BTreeMap<String, Vec<u8>>,
        raw_payload: Option<&[u8]>,
    ) -> Self {
        let mut hasher = Sha256::new();
        let mut summary = String::new();

        match name {
            Some(name) => {
                hasher.update(b"name=");
                hasher.update(name.as_bytes());
                summary.push_str("name ");
            }
            None => hasher.update(b"name="),
        }

        // Ordered by company id, then the bytes, each length-prefixed so that
        // `{1: [2, 3]}` and `{12: [3]}` cannot digest alike.
        hasher.update(b"|mfg=");
        for (company, bytes) in manufacturer_data {
            hasher.update(company.to_be_bytes());
            hasher.update((bytes.len() as u16).to_be_bytes());
            hasher.update(bytes);
            summary.push_str("manufacturer_data ");
        }

        hasher.update(b"|svc=");
        for (uuid, bytes) in service_data {
            hasher.update(uuid.as_bytes());
            hasher.update((bytes.len() as u16).to_be_bytes());
            hasher.update(bytes);
            summary.push_str("service_data ");
        }

        match raw_payload {
            Some(bytes) => {
                hasher.update(b"|raw=");
                hasher.update((bytes.len() as u32).to_be_bytes());
                hasher.update(bytes);
                summary.push_str("raw_payload");
            }
            // A backend that exposes no bytes must not digest the same as one that
            // exposes an empty payload: "not available" and "advertised nothing"
            // are different claims.
            None => hasher.update(b"|raw=absent"),
        }

        if summary.is_empty() {
            summary.push_str("no advertisement content");
        }
        let summary = summary.trim_end().to_string();

        Self {
            digest: hasher.finalize().into(),
            summary,
        }
    }

    /// What the digest was computed from, in words.
    ///
    /// Logged with a `ContentChanged` record: "the advertisement changed" is only
    /// worth reading if it says which part of it did.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Why the node is recording an occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordReason {
    /// First report of this device since the node last saw it go.
    NewDevice,
    /// An event arrived for a present device whose window had elapsed, with the
    /// same contents as the last record.
    WindowElapsed,
    /// An event arrived with contents that differ from the last record. Reported
    /// at the first opportunity the window allows, so it never means an extra row
    /// inside a window.
    ContentChanged,
    /// The node re-read a present device from the monitor because its window had
    /// elapsed and nothing had been reported for it.
    Reobservation,
}

/// Why the node is *not* recording an occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuppressReason {
    /// Inside the window.
    InsideWindow,
    /// Not held present, so there is nothing to observe: presence expired, an
    /// absence event removed it, or the caller named a device never sighted.
    NotPresent,
}

/// The policy's answer to "may I write a row for this device?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Write an occurrence.
    Record(RecordReason),
    /// Drop it. `until` is how long remains before the device is eligible again,
    /// and is zero when it will not become eligible on its own.
    Suppress {
        /// Why.
        reason: SuppressReason,
        /// Time until the window reopens.
        until: Duration,
    },
}

impl Decision {
    /// Whether this decision means writing a row.
    pub fn is_record(&self) -> bool {
        matches!(self, Decision::Record(_))
    }
}

impl RecordReason {
    /// The reason in words, for the log line that says a row is being written.
    pub fn label(&self) -> &'static str {
        match self {
            RecordReason::NewDevice => "a new device",
            RecordReason::WindowElapsed => "a window of presence",
            RecordReason::ContentChanged => "changed advertisement contents",
            RecordReason::Reobservation => "a re-observation",
        }
    }
}

/// Counters describing what the policy has decided.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObservationStats {
    /// Devices currently held as present.
    pub present: usize,
    /// Events offered to the policy.
    pub sightings: u64,
    /// Rows the policy approved, by event or by re-observation.
    pub recorded: u64,
    /// Events declined because the window was still closed.
    pub suppressed: u64,
    /// Approvals that came from the re-observation timer rather than an event.
    pub reobservations: u64,
    /// Advertisement-content changes held for the next record.
    pub content_changes_held: u64,
    /// Presence entries expired without an absence event.
    pub presence_expired: u64,
    /// Re-observations abandoned because the monitor no longer had the device.
    pub reobservation_dropped: u64,
    /// Devices dropped by an explicit absence event.
    pub removed: u64,
}

/// What the policy remembers about one device.
#[derive(Clone, Debug)]
struct Presence {
    /// The identifier the backend reported, so a re-observation can ask the
    /// monitor for the same device. The policy never decides on it.
    device_id: String,
    /// The last time anything reported this device. Presence is a function of
    /// being talked about, so this is the only clock that keeps a device alive.
    last_seen: std::time::Instant,
    /// The contents as of the last row written for this device.
    recorded_content: AdvertisementContent,
    /// Raised when contents changed while the window was closed, so the news
    /// reaches the next record instead of being suppressed into oblivion.
    pending_change: bool,
}

/// The node's occurrence-sampling policy.
///
/// Decides, per device, whether a report is worth an occurrence, and which
/// present devices are owed a fresh observation. See the [module
/// docs](self) for what the two bounds mean and why a re-observation has to be a
/// new reading.
///
/// # Thread safety
///
/// Shareable behind an `Arc`. Decisions for one device take that device's entry
/// lock and, while holding it, ask the rate limiter about the window; the reverse
/// order never occurs, so no path can deadlock against another.
pub struct ObservationPolicy {
    /// The window, and the only record of when a device was last written.
    rate_limiter: RateLimiter,

    /// How long a device counts as present after the last report for it.
    presence_timeout: Duration,

    /// What the presence map is keyed by, for the operator-facing summary.
    presence: dashmap::DashMap<Vec<u8>, Presence>,

    sightings: AtomicU64,
    /// Records that came from the timer rather than an event. The window's own
    /// counters — how many decisions were allowed and how many refused — are read
    /// from the limiter that made them rather than mirrored here.
    reobservations: AtomicU64,
    content_changes_held: AtomicU64,
    presence_expired: AtomicU64,
    reobservation_dropped: AtomicU64,
    removed: AtomicU64,
}

impl std::fmt::Debug for ObservationPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservationPolicy")
            .field("window", &self.window())
            .field("presence_timeout", &self.presence_timeout)
            .field("present", &self.presence.len())
            .finish_non_exhaustive()
    }
}

impl ObservationPolicy {
    /// Create a policy with a sampling window, a presence trust, and the clock
    /// both are measured on.
    ///
    /// The clock is the node's: the window is an interval and must not move when
    /// the wall clock does, while the timestamps written into rows come from the
    /// same clock's wall reading. Handing the policy a different clock than the
    /// node stamps with would let the two disagree about what "now" is.
    ///
    /// # Panics
    ///
    /// If `window` is zero. A zero window means "record every advertisement",
    /// which is the unbounded write rate the window exists to prevent; a node
    /// configured that way is misconfigured, not policy-driven.
    pub fn new(window: Duration, presence_timeout: Duration, clock: Arc<dyn Clock>) -> Self {
        assert!(
            !window.is_zero(),
            "the sampling window must be greater than zero"
        );

        let mut config = RateLimiterConfig::with_threshold(window);
        // The presence map bounds how many devices the node tracks, so the
        // limiter is not asked to evict as well: an entry evicted there while its
        // device stayed present would reopen that device's window early.
        config.max_cache_size = None;

        Self {
            rate_limiter: RateLimiter::with_clock(config, clock),
            presence_timeout,
            presence: dashmap::DashMap::new(),
            sightings: AtomicU64::new(0),
            reobservations: AtomicU64::new(0),
            content_changes_held: AtomicU64::new(0),
            presence_expired: AtomicU64::new(0),
            reobservation_dropped: AtomicU64::new(0),
            removed: AtomicU64::new(0),
        }
    }

    /// The window: at most one occurrence per device per this interval.
    pub fn window(&self) -> Duration {
        Duration::from_millis(self.rate_limiter.stats().threshold_ms)
    }

    /// How long a device is trusted to still be present after a report.
    pub fn presence_timeout(&self) -> Duration {
        self.presence_timeout
    }

    /// Offer a report about `device_hash` and ask whether to record it.
    ///
    /// A report is anything the backend says about the device — a discovery, a
    /// property change, or an advertisement that changed nothing. The first report
    /// of a device is always recorded; after that the window decides, with a
    /// content difference naming the next record
    /// [`RecordReason::ContentChanged`].
    ///
    /// The `device_hash` is the derived one ([`derive_device_identity`](crate::node::derive_device_identity)),
    /// not the backend's identifier: two backends naming one device differently
    /// must not sample it twice per window.
    pub fn sighting(
        &self,
        device_hash: &[u8],
        device_id: &str,
        content: &AdvertisementContent,
    ) -> Decision {
        use dashmap::mapref::entry::Entry;
        self.sightings.fetch_add(1, Ordering::SeqCst);

        match self.presence.entry(device_hash.to_vec()) {
            Entry::Vacant(vacant) => {
                // A device with no presence entry has no window either — the two
                // are created together and dropped together — so this is the
                // record. Going through `should_store` rather than `record` is
                // what lets the limiter's counters be the policy's counters: one
                // decision, counted once, in the one place that made it.
                self.rate_limiter.should_store(device_hash);
                vacant.insert(Presence {
                    device_id: device_id.to_string(),
                    last_seen: self.rate_limiter.now(),
                    recorded_content: content.clone(),
                    pending_change: false,
                });
                Decision::Record(RecordReason::NewDevice)
            }
            Entry::Occupied(mut occupied) => {
                let presence = occupied.get_mut();
                presence.last_seen = self.rate_limiter.now();

                let changed = presence.recorded_content != *content;

                // The limiter owns the window and takes the claim in one step, so
                // two reports of one device cannot both be told to record.
                if self.rate_limiter.should_store(device_hash) {
                    presence.recorded_content = content.clone();
                    presence.pending_change = false;
                    return Decision::Record(if changed {
                        RecordReason::ContentChanged
                    } else {
                        RecordReason::WindowElapsed
                    });
                }

                // The window is closed, so the record rate does not move. News is
                // held rather than thrown away: the next record names it. Held
                // once — five reports of the same change are one change.
                if changed && !presence.pending_change {
                    presence.pending_change = true;
                    self.content_changes_held.fetch_add(1, Ordering::SeqCst);
                }
                Decision::Suppress {
                    reason: SuppressReason::InsideWindow,
                    until: self.time_until_eligible(device_hash),
                }
            }
        }
    }

    /// How long until the window reopens. Zero when it is already open.
    fn time_until_eligible(&self, device_hash: &[u8]) -> Duration {
        self.rate_limiter
            .time_since_last(device_hash)
            .map(|elapsed| self.window().saturating_sub(elapsed))
            .unwrap_or(Duration::ZERO)
    }

    /// Present devices whose window has elapsed, so a fresh observation is owed.
    ///
    /// The `device_id` is the one the backend reported, so the caller can ask the
    /// monitor for a current reading. A device last reported longer than
    /// [`presence_timeout`](Self::presence_timeout) ago is not owed one: it is
    /// gone, and a row for it would say otherwise.
    ///
    /// This is a candidate list, not a reservation — a report can arrive and take
    /// the record between here and the write.
    /// [`reobservation`](Self::reobservation) is where the record is claimed.
    pub fn due(&self) -> Vec<(Vec<u8>, String)> {
        let window = self.window();
        let mut due = Vec::new();
        for entry in self.presence.iter() {
            let presence = entry.value();
            if self.presence_expired_for(presence) {
                continue;
            }
            // A device the limiter has no entry for is owed a row: with no
            // timestamp there is no window still running.
            let owed = match self.rate_limiter.time_since_last(entry.key()) {
                None => true,
                Some(elapsed) => elapsed >= window,
            };
            if owed {
                due.push((entry.key().clone(), presence.device_id.clone()));
            }
        }
        due
    }

    /// Claim the record for a fresh reading of a present device.
    ///
    /// The timer's counterpart to [`sighting`](Self::sighting): the node asked the
    /// monitor for a device [`due`](Self::due) named, and the monitor still had
    /// it. `content` is that reading, which becomes the baseline the next report
    /// is compared against.
    ///
    /// The window is re-checked rather than assumed, because an advertisement can
    /// arrive — and record — between naming a device and storing its
    /// re-observation. Without the re-check the two paths would write two rows
    /// inside one window, which is the bound the whole policy exists to hold. A
    /// caller holding a reading must therefore claim it before writing, and give
    /// up when the answer is not `Record`.
    ///
    /// Claiming before writing means a failed write spends the window anyway. That
    /// is deliberate and matches the limiter's own semantics: reserving the record
    /// until the insert succeeds would let a slow database reopen the window for
    /// every attempt and turn one device into a burst.
    pub fn reobservation(&self, device_hash: &[u8], content: &AdvertisementContent) -> Decision {
        let Some(mut entry) = self.presence.get_mut(device_hash) else {
            return Decision::Suppress {
                reason: SuppressReason::NotPresent,
                until: Duration::ZERO,
            };
        };

        if self.presence_expired_for(entry.value()) {
            return Decision::Suppress {
                reason: SuppressReason::NotPresent,
                until: Duration::ZERO,
            };
        }

        if !self.rate_limiter.should_store(device_hash) {
            let until = self
                .rate_limiter
                .time_since_last(device_hash)
                .map(|elapsed| self.window().saturating_sub(elapsed))
                .unwrap_or(Duration::ZERO);
            return Decision::Suppress {
                reason: SuppressReason::InsideWindow,
                until,
            };
        }

        let presence = entry.value_mut();
        presence.recorded_content = content.clone();
        presence.pending_change = false;
        self.reobservations.fetch_add(1, Ordering::SeqCst);
        Decision::Record(RecordReason::Reobservation)
    }

    /// The monitor no longer has this device, so it cannot be re-observed.
    ///
    /// Drops it from presence. Keeping it would mean sampling a device the node
    /// cannot read, and the next row would describe a reading that does not exist.
    pub fn reobservation_failed(&self, device_hash: &[u8]) {
        if self.presence.remove(device_hash).is_some() {
            self.rate_limiter.forget(device_hash);
            self.reobservation_dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// An absence event for `device_hash`.
    ///
    /// The window is dropped with the device: when it comes back, its return is a
    /// new co-presence, not the tail of the old one, and it is worth a row
    /// whatever the clock says.
    pub fn removed(&self, device_hash: &[u8]) -> bool {
        let removed = self.presence.remove(device_hash).is_some();
        if removed {
            self.rate_limiter.forget(device_hash);
            self.removed.fetch_add(1, Ordering::SeqCst);
        }
        removed
    }

    /// Forget devices nobody has reported for longer than
    /// [`presence_timeout`](Self::presence_timeout).
    ///
    /// Returns how many were dropped. A backend that simply stops reporting a
    /// device that left range would otherwise grow presence forever and keep
    /// sampling something that is not there.
    pub fn prune(&self) -> usize {
        let stale: Vec<Vec<u8>> = self
            .presence
            .iter()
            .filter(|entry| self.presence_expired_for(entry.value()))
            .map(|entry| entry.key().clone())
            .collect();

        for key in &stale {
            if self.presence.remove(key).is_some() {
                self.rate_limiter.forget(key);
            }
        }
        if !stale.is_empty() {
            self.presence_expired
                .fetch_add(stale.len() as u64, Ordering::SeqCst);
        }
        stale.len()
    }

    /// Whether the device is held present, whatever its window says.
    pub fn is_present(&self, device_hash: &[u8]) -> bool {
        self.presence
            .get(device_hash)
            .map(|entry| !self.presence_expired_for(entry.value()))
            .unwrap_or(false)
    }

    /// Whether the contents differ from the last record, held for the next one.
    ///
    /// This is how a caller tells that a suppressed change is waiting rather than
    /// lost.
    pub fn has_pending_change(&self, device_hash: &[u8]) -> bool {
        self.presence
            .get(device_hash)
            .map(|entry| entry.value().pending_change)
            .unwrap_or(false)
    }

    /// Statistics for the operator-facing summary.
    pub fn stats(&self) -> ObservationStats {
        let limiter = self.rate_limiter.stats();
        ObservationStats {
            present: self.presence.len(),
            sightings: self.sightings.load(Ordering::SeqCst),
            recorded: limiter.allow_count as u64,
            suppressed: limiter.deny_count as u64,
            reobservations: self.reobservations.load(Ordering::SeqCst),
            content_changes_held: self.content_changes_held.load(Ordering::SeqCst),
            presence_expired: self.presence_expired.load(Ordering::SeqCst),
            reobservation_dropped: self.reobservation_dropped.load(Ordering::SeqCst),
            removed: self.removed.load(Ordering::SeqCst),
        }
    }

    /// The window's own counters, for the operator stats the node already reports.
    pub fn rate_limiter_stats(&self) -> RateLimiterStats {
        self.rate_limiter.stats()
    }

    /// Forget everything.
    pub fn clear(&self) {
        self.presence.clear();
        self.rate_limiter.clear();
    }

    /// Whether the last report for this device is older than the trust period.
    fn presence_expired_for(&self, presence: &Presence) -> bool {
        self.rate_limiter.now().duration_since(presence.last_seen) > self.presence_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64;

    /// A clock the tests move by hand.
    ///
    /// The two readings advance together, so a test says "an hour later" once and
    /// the policy's interval arithmetic sees exactly that: no sleeping, and no
    /// assertion that depends on how fast the machine running it happens to be.
    struct SteadyClock {
        elapsed_micros: AtomicI64,
        base: std::time::Instant,
        wall_offset_ms: AtomicI64,
    }

    impl SteadyClock {
        fn new() -> Self {
            Self {
                elapsed_micros: AtomicI64::new(0),
                base: std::time::Instant::now(),
                wall_offset_ms: AtomicI64::new(0),
            }
        }

        fn advance(&self, delta: Duration) {
            self.elapsed_micros
                .fetch_add(delta.as_micros() as i64, Ordering::SeqCst);
        }

        /// Move the wall clock alone, leaving the monotonic reading where it was:
        /// an NTP correction, or a laptop closing its lid and opening it again.
        fn jump_wall_clock(&self, delta: Duration) {
            self.wall_offset_ms.fetch_add(
                chrono::Duration::from_std(delta)
                    .expect("a jump that fits")
                    .num_milliseconds(),
                Ordering::SeqCst,
            );
        }

        fn elapsed(&self) -> Duration {
            Duration::from_micros(self.elapsed_micros.load(Ordering::SeqCst) as u64)
        }
    }

    impl Clock for SteadyClock {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            use chrono::TimeZone;
            let wall_base = 1_700_000_000_000 + self.wall_offset_ms.load(Ordering::SeqCst);
            chrono::Utc
                .timestamp_millis_opt(wall_base)
                .single()
                .expect("a timestamp the tests can use")
                + chrono::Duration::from_std(self.elapsed()).expect("an elapsed time that fits")
        }

        fn monotonic(&self) -> std::time::Instant {
            self.base + self.elapsed()
        }
    }

    /// The window the tests use: long enough that "inside" and "past" are obvious.
    const WINDOW: Duration = Duration::from_secs(15);
    const PRESENCE: Duration = Duration::from_secs(45);

    fn clock() -> Arc<SteadyClock> {
        Arc::new(SteadyClock::new())
    }

    fn policy(clock: Arc<SteadyClock>) -> ObservationPolicy {
        ObservationPolicy::new(WINDOW, PRESENCE, clock)
    }

    fn content(name: &str) -> AdvertisementContent {
        AdvertisementContent::new(Some(name), &BTreeMap::new(), &BTreeMap::new(), None)
    }

    fn content_with_mfg(company: u16, bytes: Vec<u8>) -> AdvertisementContent {
        let mut mfg = BTreeMap::new();
        mfg.insert(company, bytes);
        AdvertisementContent::new(Some("Beacon"), &mfg, &BTreeMap::new(), None)
    }

    fn device_a() -> AdvertisementContent {
        content("Device A")
    }

    #[test]
    fn the_first_sighting_of_a_device_is_recorded() {
        let policy = policy(clock());

        assert_eq!(
            policy.sighting(b"a", "AA:BB:CC:DD:EE:FF", &device_a()),
            Decision::Record(RecordReason::NewDevice)
        );
    }

    #[test]
    fn a_stationary_device_is_recorded_again_once_the_window_elapses() {
        // The B14 case at the level of the policy: a device that never changes
        // anything about what it advertises. Before the policy it was recorded on
        // discovery and then never again, because the one field the node watched —
        // RSSI — was not moving.
        let clock = clock();
        let policy = policy(clock.clone());
        let seen = || policy.sighting(b"a", "AA:BB:CC:DD:EE:FF", &device_a());

        assert_eq!(seen(), Decision::Record(RecordReason::NewDevice));

        let mut inside_window = Duration::ZERO;
        for _ in 0..100 {
            clock.advance(Duration::from_millis(100));
            inside_window += Duration::from_millis(100);
            let decision = seen();
            assert!(
                matches!(decision, Decision::Suppress { .. }),
                "inside the window a sighting is suppressed, got {decision:?}"
            );
        }
        // A hundred sightings is still only ten seconds of a fifteen-second
        // window, so none of them should have recorded.
        assert_eq!(inside_window, Duration::from_secs(10));
        assert_eq!(policy.stats().recorded, 1);

        clock.advance(WINDOW - inside_window + Duration::from_millis(1));
        assert_eq!(seen(), Decision::Record(RecordReason::WindowElapsed));
    }

    #[test]
    fn the_record_rate_is_independent_of_how_often_the_radio_reports() {
        // Three beacons at three sighting rates must produce the same number of
        // records over the same span. This is the inversion the defect created:
        // volume followed the report rate, and with it signal jitter.
        let clock = clock();
        let policy = policy(clock.clone());

        let span = Duration::from_secs(15 * 60); // 60 windows
        for (hz, label) in [(1, "slow"), (10, "steady"), (100, "chatty")] {
            let hash = label.as_bytes().to_vec();
            let step = Duration::from_micros(1_000_000 / hz);
            let mut advanced = Duration::ZERO;
            let mut records = 0;
            while advanced < span {
                if policy.sighting(&hash, label, &content(label)).is_record() {
                    records += 1;
                }
                clock.advance(step);
                advanced += step;
            }
            // One for the first sighting plus one per elapsed window; the tolerance
            // absorbs each beacon's sub-window cadence drift.
            assert!(
                (58..=61).contains(&records),
                "{label} at {hz} Hz recorded {records} times in 60 windows"
            );
        }
    }

    #[test]
    fn a_sighting_inside_the_window_never_buys_an_extra_record() {
        let clock = clock();
        let policy = policy(clock.clone());

        assert!(policy.sighting(b"a", "id", &device_a()).is_record());
        clock.advance(Duration::from_millis(14_999));
        assert!(!policy.sighting(b"a", "id", &device_a()).is_record());
        clock.advance(Duration::from_millis(1));
        assert!(policy.sighting(b"a", "id", &device_a()).is_record());
    }

    #[test]
    fn each_device_has_its_own_window() {
        let clock = clock();
        let policy = policy(clock.clone());

        assert!(policy.sighting(b"a", "id-a", &content("A")).is_record());
        // B is unseen, so A's window cannot hold it back.
        assert!(policy.sighting(b"b", "id-b", &content("B")).is_record());
        assert!(!policy.sighting(b"a", "id-a", &content("A")).is_record());
    }

    #[test]
    fn a_content_change_is_held_not_lost_and_then_recorded() {
        // A sensor beacon that rewrites its payload every second is exactly the
        // device the window exists to bound, so the change does not buy a row
        // inside the window. It must not be silently dropped either: the next
        // record says it was a change.
        let clock = clock();
        let policy = policy(clock.clone());

        assert!(policy
            .sighting(b"a", "id", &content_with_mfg(0x004C, vec![0x01]))
            .is_record());

        clock.advance(Duration::from_secs(1));
        let changed = content_with_mfg(0x004C, vec![0x02]);
        assert!(
            !policy.sighting(b"a", "id", &changed).is_record(),
            "a content change does not bypass the window"
        );
        assert!(policy.has_pending_change(b"a"), "the news is held");
        assert_eq!(policy.stats().content_changes_held, 1);

        clock.advance(WINDOW - Duration::from_secs(1));
        assert_eq!(
            policy.sighting(b"a", "id", &changed),
            Decision::Record(RecordReason::ContentChanged),
            "and it is named when the window allows the record"
        );
        assert!(!policy.has_pending_change(b"a"));
    }

    #[test]
    fn a_change_held_over_several_suppressed_reports_is_still_held() {
        // Repeated reports of the same changed content must not forget the change,
        // and must not count it as news each time.
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &content_with_mfg(0x004C, vec![0x01]));

        let changed = content_with_mfg(0x004C, vec![0x02]);
        for _ in 0..5 {
            clock.advance(Duration::from_secs(2));
            policy.sighting(b"a", "id", &changed);
        }
        assert!(policy.has_pending_change(b"a"));
        assert_eq!(
            policy.stats().content_changes_held,
            1,
            "the same change is one change"
        );

        clock.advance(WINDOW - Duration::from_secs(10) + Duration::from_millis(1));
        assert_eq!(
            policy.sighting(b"a", "id", &changed),
            Decision::Record(RecordReason::ContentChanged)
        );
    }

    #[test]
    fn a_rssi_wobble_is_not_a_content_change() {
        // RSSI is not in the content digest at all, which is the mechanical reason
        // jitter stopped driving the record rate. The backend reports an RSSI
        // change as a DeviceUpdated; the policy sees one unchanged content and one
        // ordinary window.
        let clock = clock();
        let policy = policy(clock.clone());

        assert!(policy.sighting(b"a", "id", &device_a()).is_record());
        for _ in 0..20 {
            clock.advance(Duration::from_millis(200));
            assert!(!policy.sighting(b"a", "id", &device_a()).is_record());
        }
        assert_eq!(policy.stats().content_changes_held, 0);
    }

    #[test]
    fn a_present_device_becomes_due_when_its_window_elapses() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "AA:BB:CC:DD:EE:FF", &device_a());

        assert!(policy.due().is_empty(), "just recorded, nothing due");

        clock.advance(WINDOW);
        assert_eq!(
            policy.due(),
            vec![(b"a".to_vec(), "AA:BB:CC:DD:EE:FF".to_string())],
            "present and its window elapsed, so it is due"
        );
    }

    #[test]
    fn a_device_nobody_has_reported_for_is_not_due() {
        // It is gone. Writing a row would claim the device was observed, and the
        // node has no way to know that.
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        clock.advance(PRESENCE + Duration::from_secs(1));
        assert!(policy.due().is_empty());
        assert!(!policy.is_present(b"a"));
    }

    #[test]
    fn a_device_the_monitor_lost_is_dropped_not_invented() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        policy.reobservation_failed(b"a");

        assert!(!policy.is_present(b"a"), "no reading, no presence");
        assert!(policy.due().is_empty(), "and nothing to sample next tick");
        assert_eq!(policy.stats().reobservation_dropped, 1);
    }

    #[test]
    fn an_absence_event_removes_the_device_immediately() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        assert!(policy.removed(b"a"));
        assert!(!policy.is_present(b"a"));
        assert!(!policy.removed(b"a"), "removing twice is not two removals");
        assert_eq!(policy.stats().removed, 1);

        // The same device coming back is a new device, so it is recorded again —
        // inside the window it was removed in, because the window describes the
        // co-presence that just ended, not the one beginning now.
        assert_eq!(
            policy.sighting(b"a", "id", &device_a()),
            Decision::Record(RecordReason::NewDevice)
        );
    }

    #[test]
    fn pruning_forgets_devices_that_went_silent() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id-a", &device_a());
        clock.advance(Duration::from_secs(1));
        policy.sighting(b"b", "id-b", &content("B"));

        clock.advance(PRESENCE);

        assert_eq!(policy.prune(), 1, "A went silent, B did not");
        assert!(!policy.is_present(b"a"));
        assert!(policy.is_present(b"b"));
        assert_eq!(policy.stats().presence_expired, 1);
        assert_eq!(policy.prune(), 0, "pruning twice expires nothing twice");
    }

    #[test]
    fn a_device_that_returns_after_being_pruned_is_recorded_again() {
        // Presence expiring must not leave the window closed, or a device that
        // comes back to range is silent for another window on top of the time it
        // was already unreported.
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());
        clock.advance(PRESENCE + Duration::from_secs(1));
        assert_eq!(policy.prune(), 1);

        assert_eq!(
            policy.sighting(b"a", "id", &device_a()),
            Decision::Record(RecordReason::NewDevice)
        );
    }

    #[test]
    fn sighting_a_device_keeps_it_present() {
        let clock = clock();
        let policy = policy(clock.clone());

        for _ in 0..10 {
            clock.advance(PRESENCE / 2);
            policy.sighting(b"a", "id", &device_a());
            assert!(policy.is_present(b"a"), "a reported device stays present");
        }
        assert_eq!(policy.prune(), 0);
    }

    #[test]
    fn the_window_is_measured_monotonically_not_on_the_wall_clock() {
        // A wall-clock jump must neither freeze the node nor let it record twice:
        // the window is an interval, so it is measured on a clock that cannot go
        // backwards or skip.
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        clock.jump_wall_clock(Duration::from_secs(3600));
        assert!(
            !policy.sighting(b"a", "id", &device_a()).is_record(),
            "an hour of wall-clock jump is not an hour of elapsed sampling"
        );

        clock.advance(WINDOW);
        assert!(policy.sighting(b"a", "id", &device_a()).is_record());
    }

    #[test]
    fn the_suppression_reports_how_much_of_the_window_is_left() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        clock.advance(Duration::from_secs(5));
        match policy.sighting(b"a", "id", &device_a()) {
            Decision::Suppress {
                reason: SuppressReason::InsideWindow,
                until,
            } => assert_eq!(until, Duration::from_secs(10)),
            other => panic!("expected a suppression, got {other:?}"),
        }
    }

    #[test]
    fn a_reobservation_resets_the_window_and_the_content_baseline() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        clock.advance(WINDOW);
        let fresh = content_with_mfg(0x004C, vec![0x09]);
        assert_eq!(
            policy.reobservation(b"a", &fresh),
            Decision::Record(RecordReason::Reobservation)
        );

        assert!(policy.due().is_empty(), "the re-observation was the record");
        assert!(
            !policy.sighting(b"a", "id", &fresh).is_record(),
            "the re-read contents are the new baseline, not a change"
        );
    }

    #[test]
    fn a_reobservation_inside_a_window_an_advertisement_already_wrote_is_refused() {
        // `due` is a candidate list, not a reservation. An advertisement that
        // records between the naming and the write must not leave two rows for one
        // window — the upper bound is the whole point of the window.
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        clock.advance(WINDOW);
        assert_eq!(policy.due().len(), 1, "the timer finds it due");

        // An advertisement arrives and takes the record.
        assert_eq!(
            policy.sighting(b"a", "id", &device_a()),
            Decision::Record(RecordReason::WindowElapsed)
        );

        // The timer's read comes back after that; the window is spent, and the
        // advertisement's record started a whole new one.
        match policy.reobservation(b"a", &device_a()) {
            Decision::Suppress {
                reason: SuppressReason::InsideWindow,
                until,
            } => assert_eq!(
                until, WINDOW,
                "the advertisement just claimed the window, so none of it is left"
            ),
            other => panic!("expected the window to refuse the re-observation, got {other:?}"),
        }
        assert_eq!(policy.stats().recorded, 2, "discovery plus one window");
    }

    #[test]
    fn a_reobservation_of_a_device_that_went_silent_is_refused() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        // Past presence but not yet pruned: the device is still in the map, and a
        // row now would assert a co-presence nothing has reported.
        clock.advance(PRESENCE + Duration::from_secs(1));
        assert_eq!(
            policy.reobservation(b"a", &device_a()),
            Decision::Suppress {
                reason: SuppressReason::NotPresent,
                until: Duration::ZERO
            }
        );
        assert_eq!(policy.stats().recorded, 1, "only the discovery record");
    }

    #[test]
    fn reobservation_for_an_unknown_device_records_nothing() {
        let policy = policy(clock());
        // Nothing is present, so a late re-observation must not invent a presence
        // entry, open a window, or move the counters.
        assert_eq!(
            policy.reobservation(b"ghost", &device_a()),
            Decision::Suppress {
                reason: SuppressReason::NotPresent,
                until: Duration::ZERO
            }
        );
        assert_eq!(policy.stats().recorded, 0);
        assert_eq!(policy.stats().present, 0);
    }

    #[test]
    fn a_device_removed_while_due_is_not_re_observed() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        clock.advance(WINDOW);
        assert_eq!(policy.due().len(), 1);

        // The absence event lands before the node reaches the monitor.
        policy.removed(b"a");
        assert_eq!(
            policy.reobservation(b"a", &device_a()),
            Decision::Suppress {
                reason: SuppressReason::NotPresent,
                until: Duration::ZERO
            },
            "a device reported gone is not present to observe"
        );
    }

    #[test]
    fn reobservations_are_counted_apart_from_event_records() {
        // The operator needs to see that the timer is what produced the rows for
        // quiet devices; `recorded` alone cannot show it.
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());

        clock.advance(WINDOW);
        policy.reobservation(b"a", &device_a());
        clock.advance(WINDOW);
        policy.sighting(b"a", "id", &device_a());

        let stats = policy.stats();
        assert_eq!(stats.recorded, 3);
        assert_eq!(stats.reobservations, 1);
        assert_eq!(stats.sightings, 2);
    }

    #[test]
    fn stats_count_every_decision_once() {
        let policy = policy(clock());

        policy.sighting(b"a", "id", &device_a()); // recorded
        policy.sighting(b"a", "id", &device_a()); // suppressed
        policy.sighting(b"b", "id", &content("B")); // recorded

        let stats = policy.stats();
        assert_eq!(stats.sightings, 3);
        assert_eq!(stats.recorded, 2);
        assert_eq!(stats.suppressed, 1);
        assert_eq!(stats.present, 2);
    }

    #[test]
    fn the_window_the_policy_reports_is_the_window_it_applies() {
        // Read back from the limiter, which owns it: a policy that reported a
        // different window than it enforced would make the operator's numbers
        // meaningless.
        let policy = ObservationPolicy::new(
            Duration::from_millis(4_500),
            PRESENCE,
            Arc::new(SteadyClock::new()),
        );
        assert_eq!(policy.window(), Duration::from_millis(4_500));
        assert_eq!(policy.rate_limiter_stats().threshold_ms, 4_500);
    }

    #[test]
    fn clearing_forgets_the_window_as_well_as_presence() {
        let clock = clock();
        let policy = policy(clock.clone());
        policy.sighting(b"a", "id", &device_a());
        policy.clear();

        assert_eq!(policy.stats().present, 0);
        assert_eq!(
            policy.sighting(b"a", "id", &device_a()),
            Decision::Record(RecordReason::NewDevice),
            "the window was cleared with the presence"
        );
    }

    #[test]
    #[should_panic(expected = "the sampling window must be greater than zero")]
    fn a_zero_window_is_refused() {
        // "Record every advertisement" is not a policy, it is the unbounded write
        // rate the window exists to prevent.
        ObservationPolicy::new(Duration::ZERO, PRESENCE, clock());
    }

    #[test]
    fn content_digests_depend_on_contents_not_ordering() {
        let mut one = BTreeMap::new();
        one.insert(0x004Cu16, vec![1u8]);
        one.insert(0x00E0u16, vec![2u8]);
        let mut two = BTreeMap::new();
        two.insert(0x00E0u16, vec![2u8]);
        two.insert(0x004Cu16, vec![1u8]);

        let a = AdvertisementContent::new(Some("B"), &one, &BTreeMap::new(), None);
        let b = AdvertisementContent::new(Some("B"), &two, &BTreeMap::new(), None);
        assert_eq!(a, b, "map order is not content");

        let no_payload = AdvertisementContent::new(Some("B"), &one, &BTreeMap::new(), None);
        let empty_payload = AdvertisementContent::new(Some("B"), &one, &BTreeMap::new(), Some(&[]));
        assert_ne!(
            no_payload, empty_payload,
            "a backend with no bytes and a device advertising none are different claims"
        );

        let shorter = AdvertisementContent::new(Some("B"), &one, &BTreeMap::new(), Some(&[0x00]));
        let longer =
            AdvertisementContent::new(Some("B"), &one, &BTreeMap::new(), Some(&[0x00, 0x00]));
        assert_ne!(shorter, longer, "length is part of the digest");
    }

    #[test]
    fn the_content_summary_says_what_the_digest_covers() {
        let empty = AdvertisementContent::new(None, &BTreeMap::new(), &BTreeMap::new(), None);
        assert_eq!(empty.summary(), "no advertisement content");

        let mut svc = BTreeMap::new();
        svc.insert("0000fd87-0000-1000-8000-00805f9b34fb".to_string(), vec![1]);
        let mut mfg = BTreeMap::new();
        mfg.insert(0x004Cu16, vec![1]);
        let rich = AdvertisementContent::new(Some("B"), &mfg, &svc, Some(&[1, 2]));
        for word in ["name", "manufacturer_data", "service_data", "raw_payload"] {
            assert!(
                rich.summary().contains(word),
                "{} missing from {:?}",
                word,
                rich.summary()
            );
        }
    }
}
