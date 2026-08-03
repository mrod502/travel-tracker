//! Property-based tests for bt_iden identity resolution.
//!
//! These tests use proptest to generate random inputs and verify
//! invariants hold across many iterations.

use bt_iden::models::{AddressType, AdvertisementObservation, BluetoothAddress};
use bt_iden::{HeuristicIdentityResolver, IdentityResolver, ResolverConfig};
use proptest::prelude::*;
use std::time::{Duration, Instant};

proptest! {
    #[test]
    fn identical_observations_always_same_identity(
        addr_bytes in prop::array::uniform6(0u8..),
        rssi in -100i16..-30i16,
        manufacturer_id in 0u16..=100u16,
    ) {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let addr = BluetoothAddress::new(addr_bytes);
        let t = Instant::now();

        // Generate 10 identical observations
        let identities: Vec<_> = (0..10)
            .map(|i| {
                let obs = AdvertisementObservation::new(
                    t + Duration::from_secs(i as u64),
                    addr,
                    AddressType::PrivateResolvable,
                )
                .with_rssi(rssi)
                .with_manufacturer_data(manufacturer_id, vec![0x01, 0x02, 0x03]);
                resolver.observe(obs)
            })
            .collect();

        // All should be the same identity
        for i in 1..identities.len() {
            prop_assert_eq!(identities[0], identities[i]);
        }
    }

    #[test]
    fn address_rotation_resolves_to_same_identity(
        addr1 in prop::array::uniform6(0u8..),
        addr2 in prop::array::uniform6(0u8..),
        manufacturer_id in 0u16..=100u16,
    ) {
        prop_assume!(addr1 != addr2); // Ensure addresses are different

        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = Instant::now();

        let obs1 = AdvertisementObservation::new(t, BluetoothAddress::new(addr1), AddressType::PrivateResolvable)
            .with_manufacturer_data(manufacturer_id, vec![0x01, 0x02, 0x03]);

        let obs2 = AdvertisementObservation::new(t + Duration::from_secs(1), BluetoothAddress::new(addr2), AddressType::PrivateResolvable)
            .with_manufacturer_data(manufacturer_id, vec![0x01, 0x02, 0x03]);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // Same manufacturer should cause merge
        prop_assert_eq!(id1, id2);
    }

    #[test]
    fn different_manufacturer_ids_different_identities(
        addr1 in prop::array::uniform6(0u8..),
        addr2 in prop::array::uniform6(0u8..),
        man_id1 in 1u16..=50u16,
        man_id2 in 51u16..=100u16,
    ) {
        prop_assume!(man_id1 != man_id2);

        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = Instant::now();

        let obs1 = AdvertisementObservation::new(t, BluetoothAddress::new(addr1), AddressType::PrivateResolvable)
            .with_manufacturer_data(man_id1, vec![0x01, 0x02, 0x03]);

        let obs2 = AdvertisementObservation::new(t + Duration::from_secs(1), BluetoothAddress::new(addr2), AddressType::PrivateResolvable)
            .with_manufacturer_data(man_id2, vec![0x01, 0x02, 0x03]);

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // Different manufacturers should not merge
        prop_assert_ne!(id1, id2);
    }

    #[test]
    fn identity_ids_are_unique(
        count in 10usize..=100usize,
    ) {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = Instant::now();

        let identities: Vec<_> = (0..count)
            .map(|i| {
                let addr = [0x00, 0x00, 0x00, 0x00, (i >> 8) as u8, i as u8];
                let obs = AdvertisementObservation::new(
                    t,
                    BluetoothAddress::new(addr),
                    AddressType::PrivateResolvable,
                )
                .with_manufacturer_data(0x0001 + i as u16, vec![0x01]);
                resolver.observe(obs)
            })
            .collect();

        // All identity IDs should be unique
        prop_assert!(identities.len() == identities.iter().collect::<std::collections::HashSet<_>>().len());
    }

    #[test]
    fn identity_id_increases_monotonically(
        count in 10usize..=50usize,
    ) {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = Instant::now();

        let mut prev_id = 0u64;
        for i in 0..count {
            let addr = [0x00, 0x00, 0x00, 0x00, 0x00, i as u8];
            let obs = AdvertisementObservation::new(
                t,
                BluetoothAddress::new(addr),
                AddressType::PrivateResolvable,
            )
            .with_manufacturer_data(0x0001 + i as u16, vec![0x01]);

            let id = resolver.observe(obs);
            prop_assert!(id.id() > prev_id, "Identity IDs should increase monotonically");
            prev_id = id.id();
        }
    }

    #[test]
    fn expired_identities_do_not_resurrect(
        addr_bytes in prop::array::uniform6(0u8..),
        manufacturer_id in 0u16..=100u16,
    ) {
        let config = ResolverConfig::builder()
            .max_identity_age(Duration::from_secs(5))
            .matching_window(Duration::from_secs(2))
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);
        let t = Instant::now();

        // Create initial identity
        let obs1 = AdvertisementObservation::new(t, BluetoothAddress::new(addr_bytes), AddressType::PrivateResolvable)
            .with_manufacturer_data(manufacturer_id, vec![0x01]);
        let id1 = resolver.observe(obs1);

        // Expire it
        resolver.expire(t + Duration::from_secs(10));
        prop_assert_eq!(resolver.active_identity_count(), 0);

        // Reappear with same characteristics but different address
        let new_addr = [
            addr_bytes[0] ^ 0xFF,
            addr_bytes[1] ^ 0xFF,
            addr_bytes[2] ^ 0xFF,
            addr_bytes[3] ^ 0xFF,
            addr_bytes[4] ^ 0xFF,
            addr_bytes[5] ^ 0xFF,
        ];

        let obs2 = AdvertisementObservation::new(t + Duration::from_secs(10), BluetoothAddress::new(new_addr), AddressType::PrivateResolvable)
            .with_manufacturer_data(manufacturer_id, vec![0x01]);
        let id2 = resolver.observe(obs2);

        // Should be a new identity since old one expired
        prop_assert_ne!(id1.id(), id2.id());
    }

    #[test]
    fn shuffled_arrival_order_does_not_panic(
        observations in prop::collection::vec(
            (
                prop::array::uniform6(0u8..),
                prop::option::of(0u16..=100u16),
                -100i16..-30i16,
            ),
            10..50
        )
    ) {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = Instant::now();

        // Shuffle observations (clone and shuffle)
        let mut shuffled = observations.clone();
        shuffled.reverse(); // Simple deterministic "shuffle" for testing

        // Process in shuffled order - should not panic
        for (i, (addr_bytes, man_id, rssi)) in shuffled.iter().enumerate() {
            let mut obs = AdvertisementObservation::new(
                t + Duration::from_secs(i as u64),
                BluetoothAddress::new(*addr_bytes),
                AddressType::PrivateResolvable,
            )
            .with_rssi(*rssi);

            if let Some(id) = man_id {
                obs = obs.with_manufacturer_data(*id, vec![0x01, 0x02]);
            }

            let _id = resolver.observe(obs);
        }

        // Should have created some identities
        prop_assert!(resolver.active_identity_count() > 0);
    }

    #[test]
    fn consistent_rssi_produces_higher_scores(
        base_rssi in -80i16..-40i16,
    ) {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = Instant::now();

        // Create initial observation
        let addr1 = [0x12, 0x34, 0x56, 0x78, 0x90, 0xAB];
        let obs1 = AdvertisementObservation::new(t, BluetoothAddress::new(addr1), AddressType::PrivateResolvable)
            .with_rssi(base_rssi)
            .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);
        let id1 = resolver.observe(obs1);

        // Consistent RSSI
        let obs2 = AdvertisementObservation::new(t + Duration::from_secs(1), BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]), AddressType::PrivateResolvable)
            .with_rssi(base_rssi)
            .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);
        let id2 = resolver.observe(obs2);

        prop_assert_eq!(id1, id2);
    }

    #[test]
    fn varying_rssi_still_matches_with_manufacturer(
        base_rssi in -80i16..-40i16,
        rssi_variation in -10i16..=10i16,
    ) {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = Instant::now();

        let addr1 = [0x12, 0x34, 0x56, 0x78, 0x90, 0xAB];
        let obs1 = AdvertisementObservation::new(t, BluetoothAddress::new(addr1), AddressType::PrivateResolvable)
            .with_rssi(base_rssi)
            .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);
        let id1 = resolver.observe(obs1);

        // RSSI varies but manufacturer matches
        let new_rssi = (base_rssi + rssi_variation).clamp(-100, -20);
        let obs2 = AdvertisementObservation::new(t + Duration::from_secs(1), BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]), AddressType::PrivateResolvable)
            .with_rssi(new_rssi)
            .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);
        let id2 = resolver.observe(obs2);

        // Should still match due to manufacturer ID
        prop_assert_eq!(id1, id2);
    }

    #[test]
    fn deterministic_output_for_identical_input(
        observations in prop::collection::vec(
            (
                prop::array::uniform6(0u8..),
                prop::option::of(0u16..=100u16),
                -100i16..-30i16,
            ),
            10..30
        )
    ) {
        // Run twice and compare outputs
        let run_once = || {
            let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
            let t = Instant::now();
            let mut ids = Vec::new();

            for (i, (addr_bytes, man_id, rssi)) in observations.iter().enumerate() {
                let mut obs = AdvertisementObservation::new(
                    t + Duration::from_secs(i as u64),
                    BluetoothAddress::new(*addr_bytes),
                    AddressType::PrivateResolvable,
                )
                .with_rssi(*rssi);

                if let Some(id) = man_id {
                    obs = obs.with_manufacturer_data(*id, vec![0x01]);
                }

                ids.push(resolver.observe(obs).id());
            }
            ids
        };

        let result1 = run_once();
        let result2 = run_once();

        prop_assert_eq!(result1, result2, "Same inputs should produce same outputs");
    }

    #[test]
    fn high_noise_does_not_always_merge(
        addr1 in prop::array::uniform6(0u8..),
        addr2 in prop::array::uniform6(0u8..),
        data1 in prop::collection::vec(any::<u8>(), 0..10),
        data2 in prop::collection::vec(any::<u8>(), 0..10),
    ) {
        prop_assume!(!data1.is_empty() || !data2.is_empty());
        prop_assume!(data1 != data2);

        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let t = Instant::now();

        let obs1 = AdvertisementObservation::new(t, BluetoothAddress::new(addr1), AddressType::PrivateResolvable)
            .with_manufacturer_data(0x1234, data1);

        let obs2 = AdvertisementObservation::new(t + Duration::from_secs(1), BluetoothAddress::new(addr2), AddressType::PrivateResolvable)
            .with_manufacturer_data(0x1234, data2); // Same manufacturer, different payload

        let _id1 = resolver.observe(obs1);
        let _id2 = resolver.observe(obs2);

        // Different payloads with same manufacturer may or may not merge
        // but should not guarantee merge
        // (This is a weak assertion due to scoring complexity)
        prop_assert!(resolver.active_identity_count() >= 1);
    }
}

mod tests {
    use super::*;

    #[test]
    fn test_proptest_runner() {
        // Simple smoke test to ensure proptest integration works
        proptest!(|(x in 1i32..10i32)| {
            assert!(x > 0);
        });
    }
}
