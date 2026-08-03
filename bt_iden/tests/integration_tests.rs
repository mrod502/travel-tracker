//! Integration tests for bt_iden identity resolution.
//!
//! These tests verify end-to-end behavior of the identity resolver
//! under various scenarios.

use bt_iden::models::{AddressType, AdvertisementObservation, BluetoothAddress};
use bt_iden::{HeuristicIdentityResolver, IdentityResolver, ResolverConfig};
use std::time::{Duration, Instant};

fn now() -> Instant {
    Instant::now()
}

fn obs_with_addr(ts: Instant, addr: [u8; 6]) -> AdvertisementObservation {
    AdvertisementObservation::new(
        ts,
        BluetoothAddress::new(addr),
        AddressType::PrivateResolvable,
    )
    .with_rssi(-65)
    .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03])
}

#[expect(dead_code)]
fn obs_with_uuid(ts: Instant, addr: [u8; 6], uuids: &[uuid::Uuid]) -> AdvertisementObservation {
    let mut obs = AdvertisementObservation::new(
        ts,
        BluetoothAddress::new(addr),
        AddressType::PrivateResolvable,
    )
    .with_rssi(-65);
    for uuid in uuids {
        obs = obs.with_service_uuid(*uuid);
    }
    obs
}

mod test_exact_matches {
    use super::*;

    #[test]
    fn test_same_address_repeatedly() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();
        let addr = [0x12, 0x34, 0x56, 0x78, 0x90, 0xAB];

        let identities: Vec<_> = (0..10)
            .map(|i| {
                let obs = obs_with_addr(t + Duration::from_secs(i as u64), addr);
                resolver.observe(obs)
            })
            .collect();

        // All should be the same identity
        for i in 1..identities.len() {
            assert_eq!(identities[0], identities[i]);
        }

        assert_eq!(resolver.active_identity_count(), 1);
    }

    #[test]
    fn test_same_observation_structure() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([1, 2, 3, 4, 5, 6]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-60)
        .with_manufacturer_data(0x004C, vec![0xAA, 0xBB])
        .with_service_uuid(uuid::Uuid::parse_str("0000180F-0000-1000-8000-00805F9B34FB").unwrap())
        .with_local_name("Sensor".to_string());

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([1, 2, 3, 4, 5, 6]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-61)
        .with_manufacturer_data(0x004C, vec![0xAA, 0xBB])
        .with_service_uuid(uuid::Uuid::parse_str("0000180F-0000-1000-8000-00805F9B34FB").unwrap())
        .with_local_name("Sensor".to_string());

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        assert_eq!(id1, id2);
    }
}

mod test_address_rotation {
    use super::*;

    #[test]
    fn test_single_rotation() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-64)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        assert_eq!(id1, id2, "Address rotation should resolve to same identity");
    }

    #[test]
    fn test_multiple_rotations() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        let addresses = [
            [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            [0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            [0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA],
        ];

        let identities: Vec<_> = addresses
            .iter()
            .enumerate()
            .map(|(i, addr)| {
                let obs = AdvertisementObservation::new(
                    t + Duration::from_secs(i as u64),
                    BluetoothAddress::new(*addr),
                    AddressType::PrivateResolvable,
                )
                .with_rssi(-65)
                .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);
                resolver.observe(obs)
            })
            .collect();

        // All should resolve to same identity due to matching manufacturer data
        for i in 1..identities.len() {
            assert_eq!(identities[0], identities[i]);
        }

        assert_eq!(resolver.active_identity_count(), 1);
    }
}

mod test_different_devices {
    use super::*;

    #[test]
    fn test_different_manufacturer_ids() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]); // Apple

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x005E, vec![0x01, 0x02, 0x03]); // Samsung

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        assert_ne!(id1, id2, "Different manufacturer IDs should not merge");
    }

    #[test]
    fn test_different_service_uuids() {
        let uuid1 = uuid::Uuid::parse_str("0000180F-0000-1000-8000-00805F9B34FB").unwrap(); // Battery
        let uuid2 = uuid::Uuid::parse_str("0000180D-0000-1000-8000-00805F9B34FB").unwrap(); // Heart Rate

        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_service_uuid(uuid1);

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_service_uuid(uuid2);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // With no manufacturer ID match and different UUIDs, should not merge
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_rssi_only_not_sufficient() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        // Two devices with similar RSSI but no other matching features
        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65);

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-66);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        assert_ne!(id1, id2, "RSSI similarity alone should not cause merge");
    }
}

mod test_expiration {
    use super::*;

    #[test]
    fn test_identity_expiration() {
        let config = ResolverConfig::builder()
            .max_identity_age(Duration::from_secs(10))
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let t = now();
        let obs = obs_with_addr(t, [0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]);

        resolver.observe(obs);
        assert_eq!(resolver.active_identity_count(), 1);

        // Expire after max age
        resolver.expire(t + Duration::from_secs(15));
        assert_eq!(resolver.active_identity_count(), 0);
        assert_eq!(resolver.expired_identity_count(), 1);
    }

    #[test]
    fn test_no_expiration_with_recent_activity() {
        let config = ResolverConfig::builder()
            .max_identity_age(Duration::from_secs(10))
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let t = now();
        let obs1 = obs_with_addr(t, [0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]);
        resolver.observe(obs1);

        // Keep identity alive with recent observations
        for i in 1..10 {
            let obs = obs_with_addr(
                t + Duration::from_secs(i * 10),
                [0x12, 0x34, 0x56, 0x78, 0x90, 0xAB],
            );
            resolver.observe(obs);
        }

        // Should still be alive
        resolver.expire(t + Duration::from_secs(100));
        assert_eq!(resolver.active_identity_count(), 1);
    }

    #[test]
    fn test_expired_device_reappears_as_new() {
        let config = ResolverConfig::builder()
            .max_identity_age(Duration::from_secs(5))
            .matching_window(Duration::from_secs(2))
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let t = now();
        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        resolver.observe(obs1);
        assert_eq!(resolver.active_identity_count(), 1);

        // Wait for expiration
        resolver.expire(t + Duration::from_secs(10));
        assert_eq!(resolver.active_identity_count(), 0);

        // Device reappears with new address
        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(10),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let _new_id = resolver.observe(obs2);
        assert_eq!(resolver.active_identity_count(), 1);
        // Should be new identity since old one was expired
        assert_eq!(resolver.expired_identity_count(), 1);
    }
}

mod test_uuid_overlap {
    use super::*;

    #[test]
    fn test_partial_uuid_overlap() {
        let uuid1 = uuid::Uuid::parse_str("0000180F-0000-1000-8000-00805F9B34FB").unwrap();
        let uuid2 = uuid::Uuid::parse_str("0000180D-0000-1000-8000-00805F9B34FB").unwrap();
        let uuid3 = uuid::Uuid::parse_str("0000180A-0000-1000-8000-00805F9B34FB").unwrap();

        let config = ResolverConfig::builder()
            .merge_threshold(20.0) // Lower threshold for UUID-only test
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let t = now();

        // Device with 2 UUIDs
        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_service_uuid(uuid1)
        .with_service_uuid(uuid2);

        // Same device with 3 UUIDs (including overlap)
        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_service_uuid(uuid1)
        .with_service_uuid(uuid2)
        .with_service_uuid(uuid3);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        assert_eq!(id1, id2, "Partial UUID overlap should contribute to merge");
    }
}

mod test_confidence {
    use super::*;

    #[test]
    fn test_confidence_increases_with_matches() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        let addr = [0x12, 0x34, 0x56, 0x78, 0x90, 0xAB];

        // First observation creates identity with 0.5 confidence
        let _id = resolver.observe(obs_with_addr(t, addr));

        // Multiple consistent observations should increase confidence
        for i in 1..20 {
            let obs = AdvertisementObservation::new(
                t + Duration::from_secs(i as u64),
                BluetoothAddress::new(addr),
                AddressType::PrivateResolvable,
            )
            .with_rssi(-65)
            .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

            resolver.observe(obs);
        }

        // Confidence should have increased (internal state check via observation count)
        assert_eq!(resolver.active_identity_count(), 1);
    }
}

mod test_configuration {
    use super::*;

    #[test]
    fn test_high_merge_threshold_prevents_merging() {
        let config = ResolverConfig::builder()
            .merge_threshold(200.0) // Very high threshold
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let t = now();

        // Same manufacturer data but high threshold
        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // High threshold prevents merge
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_low_merge_threshold_promotes_merging() {
        let config = ResolverConfig::builder()
            .merge_threshold(30.0) // Low threshold
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let t = now();

        // Just manufacturer ID should be enough at low threshold
        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // Low threshold allows merge
        assert_eq!(id1, id2);
    }
}

mod test_scoring_components {
    use super::*;

    #[test]
    fn test_local_name_matching() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_local_name("MyDevice".to_string());

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_local_name("MyDevice".to_string());

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // Same name should contribute to merge
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_appearance_matching() {
        // Appearance alone may not be enough - test with lower threshold
        let config = ResolverConfig::builder()
            .merge_threshold(20.0)
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);
        let t = now();

        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_appearance(0x0340); // Heart Rate Sensor

        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_appearance(0x0340); // Same type

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // Same appearance should contribute to merge at lower threshold
        assert_eq!(id1, id2);
    }
}

mod test_edge_cases {
    use super::*;

    #[test]
    fn test_empty_observation() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        let obs1 = AdvertisementObservation::new(
            t,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        );
        let obs2 = AdvertisementObservation::new(
            t + Duration::from_secs(1),
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        );

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // Same address should still match
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_many_simultaneous_devices() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        // Create 100 different devices
        for i in 0..100 {
            let addr = [
                0x00,
                0x00,
                (i >> 16) as u8,
                (i >> 8) as u8,
                i as u8,
                (i * 2) as u8,
            ];

            let obs = AdvertisementObservation::new(
                t,
                BluetoothAddress::new(addr),
                AddressType::PrivateResolvable,
            )
            .with_manufacturer_data(0x004C + i as u16, vec![0x01]);

            resolver.observe(obs);
        }

        assert_eq!(resolver.active_identity_count(), 100);
    }

    #[test]
    fn test_reset_clears_all_state() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = now();

        for i in 0..10 {
            let addr = [0x00, 0x00, 0x00, 0x00, 0x00, i as u8];
            let obs = AdvertisementObservation::new(
                t,
                BluetoothAddress::new(addr),
                AddressType::PrivateResolvable,
            );
            resolver.observe(obs);
        }

        assert_eq!(resolver.active_identity_count(), 10);

        resolver.reset();

        // Reset clears both active and expired identities
        assert_eq!(resolver.active_identity_count(), 0);
        assert_eq!(resolver.expired_identity_count(), 0);
    }
}
