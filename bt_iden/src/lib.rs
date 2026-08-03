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
//! An [`AdvertisementObservation`](models::AdvertisementObservation) represents a single BLE
//! advertisement event. It contains normalized data independent of how it was captured
//! (BlueZ, raw HCI, pcap, etc.).
//!
//! ### Identities
//!
//! A [`DeviceIdentity`](models::DeviceIdentity) is an opaque logical identifier assigned to
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
//! use std::time::{Instant, Duration};
//!
//! // Create resolver with default configuration
//! let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
//!
//! // Create observations
//! let now = Instant::now();
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
//! resolver.expire(Instant::now() + Duration::from_secs(300));
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
//! Thresholds:
//! - **Merge ≥ 40**: Strong confidence, merge into existing identity
//! - **Possible ≥ 25**: Potential match (internal use)
//! - **Reject < 25**: Unlikely to be same device
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
//! - **False positives**: Similar devices may incorrectly merge
//! - **False negatives**: Device changes may cause splits
//! - **No persistence**: Identities are not persisted across restarts
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

pub mod config;
pub mod models;
pub mod resolver;

// Re-export main types at crate root
pub use config::ResolverConfig;
pub use models::{
    AddressType, AdvertisementObservation, BluetoothAddress, DeviceIdentity, ScoringWeights,
    ServiceData,
};
pub use resolver::{HeuristicIdentityResolver, IdentityResolver};

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

        let now = std::time::Instant::now();
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
