//! # bt_iden - Bluetooth LE Identity Resolution
//!
//! `bt_iden` is a reusable library for assigning **stable logical identities** to Bluetooth
//! Low Energy (BLE) advertisement observations. The library implements a **probabilistic identity
//! resolution engine** that groups observations likely to belong to the same physical device
//! across address rotations.
//!
//! ## Design Philosophy
//!
//! Bluetooth LE privacy features intentionally prevent reliable tracking of unpaired devices.
//! This library does **not** attempt to defeat privacy mechanisms or infer cryptographically
//! verifiable identities. Instead, it provides **best-effort inference** based on observable
//! characteristics that may remain stable:
//!
//! - Manufacturer-specific data patterns
//! - Service UUID advertisements
//! - Advertisement structure (AD field ordering)
//! - Signal strength continuity
//! - Advertisement timing patterns
//! - Device appearance and names
//!
//! ## Core Concepts
//!
//! ### Observations
//!
//! An [`AdvertisementObservation`] represents a single BLE
//! advertisement event. It contains normalized data independent of how it was captured
//! (BlueZ, raw HCI, pcap, etc.).
//!
//! ### Identities
//!
//! A [`DeviceIdentity`] is an opaque logical identifier assigned to
//! a group of observations. Identities are stable across address rotations when the resolver
//! has sufficient confidence.
//!
//! ### Resolution
//!
//! The [`IdentityResolver`] trait defines the interface for assigning identities. The
//! [`HeuristicIdentityResolver`] provides a concrete implementation using weighted scoring.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use bt_iden::{IdentityResolver, HeuristicIdentityResolver};
//! use bt_iden::config::ResolverConfig;
//! use bt_iden::models::{AdvertisementObservation, BluetoothAddress, AddressType};
//! use bt_iden::time::ObservationTime;
//! use std::time::Duration;
//!
//! // Create resolver with default configuration
//! let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
//!
//! // Create observations
//! let now = ObservationTime::now();
//! let obs1 = AdvertisementObservation::new(
//!     now,
//!     BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
//!     AddressType::PrivateResolvable,
//! )
//! .with_rssi(-65)
//! .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);
//!
//! // Assign identity
//! let identity = resolver.observe(obs1);
//! println!("Assigned identity: {}", identity);
//!
//! // Periodically expire old identities
//! resolver.expire(now + Duration::from_secs(300));
//! ```
//!
//! ## Feeding it from something lossy
//!
//! A radio stack that hands over decoded GATT properties cannot supply every
//! feature the scoring model wants, and neither can a row that was stored without
//! them. Callers say so with [`FeatureSources`], and
//! [`IdentityResolver::resolve`] reports how much of the model a decision actually
//! rested on:
//!
//! ```
//! use bt_iden::{HeuristicIdentityResolver, IdentityResolver};
//! use bt_iden::config::ResolverConfig;
//! use bt_iden::evidence::Datum;
//! use bt_iden::models::{AdvertisementObservation, AddressType, BluetoothAddress};
//! use bt_iden::time::ObservationTime;
//!
//! let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
//!
//! // A backend that never sees AD structures: the name is real, the appearance is
//! // copied from a cached GATT read rather than advertised.
//! fn observation(now: ObservationTime) -> AdvertisementObservation {
//!     AdvertisementObservation::new(
//!         now,
//!         BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
//!         AddressType::PrivateResolvable,
//!     )
//!     .with_local_name("TAG-01".to_string())
//!     .with_appearance_datum(0x01C0, Datum::Derived)
//! }
//!
//! let first = resolver.resolve(observation(ObservationTime::now()));
//! assert!(first.is_new());
//! // A brand new identity has nothing to have compared yet.
//! assert_eq!(first.outcome.coverage(), 0.0);
//!
//! let second = resolver.resolve(observation(ObservationTime::now()));
//! assert_eq!(second.identity, first.identity);
//! // The two features this feed can supply, out of the ten the model designs.
//! assert!(second.outcome.coverage() > 0.0 && second.outcome.coverage() < 0.5);
//! ```
//!
//! ## Scoring System
//!
//! The resolver uses a configurable weighted scoring system to determine matches:
//!
//! | Feature | Default Weight | Description |
//! |---------|---------------|-------------|
//! | Manufacturer ID | 40 | Exact match only |
//! | Service UUIDs | 30 | Jaccard similarity |
//! | Appearance | 15 | Exact device category |
//! | Field Layout | 15 | AD type ordering |
//! | Payload Similarity | 20 | Byte-level comparison |
//! | Time Continuity | 25 | Recency bonus |
//! | RSSI | 10 | Signal continuity |
//! | Name | 25 | Local name match |
//! | Connectable | 5 | Flag match |
//!
//! Those nine are the *designed* model and they sum to 185. What one observation can actually claim
//! is smaller, and [`FeatureSources::coverage`] is how much of the model it can feed: a feature whose
//! datum is [`Datum::Absent`] leaves the denominator entirely — an unmeasured feature is not evidence
//! of difference — and a [`Datum::Derived`] one is scored at `derived_evidence_factor` (0.5), because
//! "inferred from something else" should not carry the conviction of "advertised".
//!
//! Thresholds:
//! - **Merge ≥ 40**: Strong confidence, merge into existing identity
//! - **Possible ≥ 25**: Potential match (internal use)
//! - **Reject < 25**: Unlikely to be same device
//! - **Evidence floor 0.35** (`min_evidence_ratio`): a merge must *also* rest on at least that much of
//!   the designed model, so a low-information observation cannot clear 40 on RSSI and a timestamp
//!
//! An exact address match is separate from all of this: it carries no weight at all
//! (`Feature::weight` returns `0.0`) but scores `manufacturer_id + time_continuity` and is exempt from
//! the evidence floor, because the same address is identification rather than inference. Everything
//! else is inference, and the evidence attached to a merge — [`MatchEvidence::coverage`](models::MatchEvidence::coverage),
//! [`MatchEvidence::top_features`](models::MatchEvidence::top_features) — says how thin it was.
//!
//! ## Configuration
//!
//! ```rust
//! use bt_iden::config::ResolverConfig;
//! use bt_iden::models::ScoringWeights;
//! use std::time::Duration;
//!
//! // Using builder pattern
//! let config = ResolverConfig::builder()
//!     .merge_threshold(60.0)
//!     .matching_window(Duration::from_secs(120))
//!     .max_identity_age(Duration::from_secs(600))
//!     .debug_logging(true)
//!     .build();
//!
//! // Or using methods
//! let config = ResolverConfig::default()
//!     .with_merge_threshold(60.0)
//!     .with_weights(ScoringWeights {
//!         manufacturer_id: 40.0,
//!         ..ScoringWeights::default()
//!     });
//! ```
//!
//! ## Limitations
//!
//! - **No guarantees**: Identity resolution is probabilistic, not deterministic
//! - **Address privacy**: Modern BLE devices frequently rotate addresses
//! - **False positives**: A device that changes what it advertises can be split, and two devices that
//!   advertise *exactly* the same things are one identity. The features the device itself picks —
//!   manufacturer ID, appearance, service list, advertised name — are Direct-quality, so a difference
//!   in any of them refuses the merge (see `vetoed_feature`; `name` tolerates truncation, because an
//!   advertisement carries at most 30 bytes of local name and `Pixel 7 P` is not a rename). What is left
//!   to argue sameness with is the payload, and payload shape is a property of a *product line*, not of
//!   an individual unit — so five beacons that share a firmware default name as well as a manufacturer ID
//!   and an AD layout are indistinguishable from one beacon seen five times, and merge. A refused
//!   candidate opens its own identity rather than being scored down, and both the merge and the refusal
//!   name the features that carried them, which is what makes either reviewable.
//! - **False negatives**: Device changes may cause splits
//! - **No persistence *in this crate***: a `DeviceIdentity(u64)` counts inside one process, and that is
//!   the right scope for it. What persists lives in the consumer (`app/src/identity/`), which keys rows
//!   on a feature fingerprint instead of on these ids — so reprocessing converges on the same device
//!   while the thresholds here stay as tight as the resolver needs them to be
//!
//! ## Architecture
//!
//! ```text
//! HeuristicIdentityResolver
//!     ├── Configuration (thresholds, weights, windows)
//!     ├── PhysicalIdentity (state per inferred device)
//!     │   ├── Address history
//!     │   ├── RSSI statistics
//!     │   ├── Confidence tracking
//!     │   └── Learned features
//!     └── Scoring System
//!         ├── Manufacturer ID scorer
//!         ├── Service UUID scorer
//!         ├── Appearance scorer
//!         ├── Field layout scorer
//!         ├── Payload similarity scorer
//!         ├── Time continuity scorer
//!         ├── RSSI continuity scorer
//!         ├── Local name scorer
//!         └── Connectable scorer
//! ```
//!
//! ## Testing
//!
//! The library includes comprehensive tests:
//!
//! - Unit tests for individual scorers
//! - Integration tests for end-to-end resolution
//! - Property tests using proptest
//! - Benchmarks using Criterion
//!
//! Run tests:
//! ```bash
//! cargo test
//! cargo test -- --nocapture
//! ```
//!
//! Run benchmarks:
//! ```bash
//! cargo bench
//! ```
//!
//! ## Thread Safety
//!
//! The current implementation is **not** thread-safe. For concurrent access, wrap
//! the resolver in a `Mutex` or use per-thread instances.
//!
//! ## Future Enhancements
//!
//! The scoring system is designed for extensibility. Future versions may include:
//!
//! - Machine learning-based classifiers
//! - Persistence support
//! - Thread-safe implementation
//! - More sophisticated payload comparison
//! - Cross-device correlation

#![forbid(unsafe_code)]
#![warn(missing_docs, rustdoc::missing_crate_level_docs)]

pub mod ad;
pub mod config;
pub mod evidence;
pub mod models;
pub mod resolver;
pub mod time;

// Re-export main types at crate root
pub use config::ResolverConfig;
pub use evidence::{Coverage, Datum, Feature, FeatureSources};
pub use models::{
    AddressType, AdvertisementObservation, BluetoothAddress, DeviceIdentity, FeatureScore,
    MatchEvidence, ScoringWeights, ServiceData,
};
pub use resolver::{HeuristicIdentityResolver, IdentityResolver, Outcome, Resolution};
pub use time::ObservationTime;

/// Library version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn test_basic_resolution() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());

        let now = ObservationTime::now();
        let obs1 = AdvertisementObservation::new(
            now,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let obs2 = AdvertisementObservation::new(
            now + std::time::Duration::from_secs(1),
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-66)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        assert_eq!(id1, id2);
        assert_eq!(resolver.active_identity_count(), 1);
    }
}
