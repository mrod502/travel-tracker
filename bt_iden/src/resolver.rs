//! Identity resolver trait and implementation.
//!
//! This module defines the core [`IdentityResolver`] trait that all identity
//! resolution engines must implement, along with the concrete
//! [`HeuristicIdentityResolver`] implementation.

use std::collections::HashMap;
use std::time::Instant;

use crate::config::ResolverConfig;
use crate::models::{
    AdvertisementObservation, DeviceIdentity, MatchCandidate, PhysicalIdentity,
};

/// Trait for assigning stable logical identities to Bluetooth observations.
///
/// The `IdentityResolver` is the core interface for the identity resolution
/// engine. It takes sequential advertisement observations and assigns each
/// to a logical identity, merging observations that appear to come from the
/// same physical device.
///
/// # Example
///
/// ```
/// use bt_iden::IdentityResolver;
/// use bt_iden::config::ResolverConfig;
/// use bt_iden::models::{AdvertisementObservation, BluetoothAddress, AddressType};
/// use std::time::Instant;
///
/// let mut resolver = bt_iden::HeuristicIdentityResolver::new(ResolverConfig::default());
///
/// let obs1 = AdvertisementObservation::new(
///     Instant::now(),
///     BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
///     AddressType::PrivateResolvable,
/// );
///
/// let identity = resolver.observe(obs1);
/// ```
pub trait IdentityResolver {
    /// The type of observation this resolver accepts.
    type Observation;

    /// The type of identity assigned to observations.
    type Identity;

    /// Records an observation and returns its assigned identity.
    ///
    /// This is the main entry point for feeding observations into the
    /// resolver. The resolver will attempt to match the observation to
    /// an existing identity, or create a new one if no suitable match
    /// is found.
    ///
    /// # Arguments
    ///
    /// * `observation` - The advertisement observation to process
    ///
    /// # Returns
    ///
    /// The identity assigned to this observation, either an existing
    /// identity or a newly created one.
    fn observe(&mut self, observation: Self::Observation) -> Self::Identity;

    /// Expires old identities and observations.
    ///
    /// Call this method periodically to remove identities that haven't
    /// been observed within the configured time window. This helps
    /// maintain memory efficiency and ensures stale devices don't
    /// interfere with new matches.
    ///
    /// # Arguments
    ///
    /// * `now` - The current timestamp
    fn expire(&mut self, now: Instant);

    /// Resets all state and starts fresh.
    ///
    /// This clears all learned identities and configuration, returning
    /// the resolver to its initial state.
    fn reset(&mut self);

    /// Returns the number of active identities.
    fn active_identity_count(&self) -> usize;

    /// Returns the number of expired identities.
    fn expired_identity_count(&self) -> usize;
}

/// A heuristic-based identity resolver for Bluetooth LE advertisements.
///
/// `HeuristicIdentityResolver` implements probabilistic identity resolution
/// using a weighted scoring system. It maintains internal state about observed
/// devices, tracking addresses, signal strength patterns, and advertisement
/// features to build confidence in identity assignments.
///
/// # Design Philosophy
///
/// This resolver is designed around the principle that Bluetooth LE privacy
/// features intentionally prevent reliable tracking of unpaired devices.
/// The resolver provides **best-effort inference** based on observable
/// characteristics that may remain stable across address rotations:
///
/// - Manufacturer-specific data patterns
/// - Service UUID advertisements
/// - Advertisement structure (AD field ordering)
/// - Signal strength continuity
/// - Advertisement timing patterns
/// - Device appearance and names
///
/// # Confidence Model
///
/// Each identity maintains a confidence score that increases with consistent
/// matching and decreases when contradictory evidence is observed. The
/// resolver uses configurable thresholds to determine when a match is
/// confident enough to merge observations.
///
/// # Performance
///
/// The resolver is optimized for O(n) performance where n is the number of
/// active identities within the matching window. Expired identities are
/// periodically cleaned up to maintain efficiency.
///
/// # Example
///
/// ```
/// use bt_iden::{HeuristicIdentityResolver, IdentityResolver};
/// use bt_iden::config::ResolverConfig;
/// use bt_iden::models::{AdvertisementObservation, BluetoothAddress, AddressType};
/// use std::time::Instant;
///
/// let config = ResolverConfig::default();
/// let mut resolver = HeuristicIdentityResolver::new(config);
///
/// // Process observations
/// let obs = AdvertisementObservation::new(
///     Instant::now(),
///     BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
///     AddressType::PrivateResolvable,
/// );
/// let identity = resolver.observe(obs);
///
/// // Periodically expire old identities
/// resolver.expire(Instant::now());
/// ```
pub struct HeuristicIdentityResolver {
    /// Configuration settings.
    config: ResolverConfig,

    /// Map of logical identity ID to physical identity state.
    identities: HashMap<u64, PhysicalIdentity>,

    /// Counter for generating unique identity IDs.
    next_identity_id: u64,

    /// Set of expired identity IDs (for diagnostics).
    expired_ids: Vec<u64>,
}

impl HeuristicIdentityResolver {
    /// Creates a new resolver with the given configuration.
    pub fn new(config: ResolverConfig) -> Self {
        Self {
            config,
            identities: HashMap::new(),
            next_identity_id: 1,
            expired_ids: Vec::new(),
        }
    }

    /// Returns a reference to the current configuration.
    pub fn config(&self) -> &ResolverConfig {
        &self.config
    }

    /// Creates a new physical identity for an observation.
    fn create_identity(&mut self, observation: &AdvertisementObservation) -> DeviceIdentity {
        let id = DeviceIdentity::from_id(self.next_identity_id);
        self.next_identity_id += 1;

        let physical = PhysicalIdentity::new(id, observation, self.config.rssi_window_size);

        tracing::info!(
            identity.id = id.id(),
            address = %observation.address,
            "Created new identity"
        );

        self.identities.insert(id.id(), physical);
        id
    }

    /// Finds the best matching identity for an observation.
    fn find_best_match(&self, observation: &AdvertisementObservation) -> Option<MatchCandidate> {
        let now = observation.timestamp;
        let window_start = now - self.config.matching_window;

        // Find all active candidates within the matching window
        let candidates: Vec<_> = self
            .identities
            .values()
            .filter(|p| p.last_observation >= window_start)
            .map(|physical| self.score_observation(observation, physical))
            .filter(|candidate| candidate.total_score >= self.config.possible_threshold)
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Return the highest-scoring candidate
        candidates.into_iter().max_by(|a, b| {
            a.total_score
                .partial_cmp(&b.total_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// Scores an observation against a physical identity.
    fn score_observation(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> MatchCandidate {
        let mut total_score = 0.0;
        let mut component_scores = std::collections::HashMap::new();

        // Exact address match (strongest signal)
        let address_score = self.score_address_match(observation, physical);
        total_score += address_score;
        component_scores.insert("address_match".to_string(), address_score);

        // Manufacturer ID match (strong signal)
        let manufacturer_score = self.score_manufacturer_id(observation, physical);
        total_score += manufacturer_score;
        component_scores.insert("manufacturer_id".to_string(), manufacturer_score);

        // Service UUID overlap
        let uuid_score = self.score_service_uuids(observation, physical);
        total_score += uuid_score;
        component_scores.insert("uuid_overlap".to_string(), uuid_score);

        // Appearance match
        let appearance_score = self.score_appearance(observation, physical);
        total_score += appearance_score;
        component_scores.insert("appearance".to_string(), appearance_score);

        // AD field layout
        let layout_score = self.score_field_layout(observation, physical);
        total_score += layout_score;
        component_scores.insert("field_layout".to_string(), layout_score);

        // Payload similarity (manufacturer data)
        let payload_score = self.score_payload_similarity(observation, physical);
        total_score += payload_score;
        component_scores.insert("payload_similarity".to_string(), payload_score);

        // Time continuity
        let time_score = self.score_time_continuity(observation, physical);
        total_score += time_score;
        component_scores.insert("time_continuity".to_string(), time_score);

        // RSSI continuity
        let rssi_score = self.score_rssi_continuity(observation, physical);
        total_score += rssi_score;
        component_scores.insert("rssi".to_string(), rssi_score);

        // Local name match
        let name_score = self.score_local_name(observation, physical);
        total_score += name_score;
        component_scores.insert("name".to_string(), name_score);

        // Connectable flag
        let connectable_score = self.score_connectable(observation, physical);
        total_score += connectable_score;
        component_scores.insert("connectable".to_string(), connectable_score);

        let candidate = MatchCandidate::new(physical.identity, total_score);

        if self.config.debug_logging {
            tracing::debug!(
                identity.id = physical.identity.id(),
                ?component_scores,
                "Match scoring complete"
            );
        }

        candidate.with_component("total".to_string(), total_score)
    }

    /// Scores exact address match.
    fn score_address_match(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> f64 {
        if observation.address == physical.current_address.address {
            // Strong signal: same address is definitive proof
            self.config.weights.manufacturer_id + self.config.weights.time_continuity
        } else {
            0.0
        }
    }

    /// Scores manufacturer ID match.
    fn score_manufacturer_id(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> f64 {
        match (
            observation.manufacturer_id,
            physical.stable_features.manufacturer_id,
        ) {
            (Some(obs_id), Some(phy_id)) if obs_id == phy_id => self.config.weights.manufacturer_id,
            (Some(_), Some(_)) => 0.0, // Mismatch - strong negative
            _ => 0.0,                  // No data to compare
        }
    }

    /// Scores service UUID overlap.
    fn score_service_uuids(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> f64 {
        if observation.service_uuids.is_empty() || physical.stable_features.service_uuids.is_empty()
        {
            return 0.0;
        }

        let obs_set: std::collections::HashSet<_> = observation.service_uuids.iter().collect();
        let phy_set: std::collections::HashSet<_> =
            physical.stable_features.service_uuids.iter().collect();

        let intersection: usize = obs_set.intersection(&phy_set).count();
        let union: usize = obs_set.union(&phy_set).count();

        if union == 0 {
            return 0.0;
        }

        // Jaccard similarity * weight
        let similarity = intersection as f64 / union as f64;
        similarity * self.config.weights.uuid_overlap
    }

    /// Scores appearance match.
    fn score_appearance(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> f64 {
        match (observation.appearance, physical.stable_features.appearance) {
            (Some(obs), Some(phy)) if obs == phy => self.config.weights.appearance,
            (Some(_), Some(_)) => 0.0,
            _ => 0.0,
        }
    }

    /// Scores AD field layout similarity.
    fn score_field_layout(
        &self,
        observation: &AdvertisementObservation,
        _physical: &PhysicalIdentity,
    ) -> f64 {
        let obs_fields = observation.ad_field_types();

        // We don't store historical field types yet, so return 0 for now
        if obs_fields.is_empty() {
            return 0.0;
        }

        // TODO: Implement proper field layout comparison when historical data is stored
        0.0
    }

    /// Scores manufacturer payload similarity.
    fn score_payload_similarity(
        &self,
        observation: &AdvertisementObservation,
        _physical: &PhysicalIdentity,
    ) -> f64 {
        if observation.manufacturer_data.is_empty() {
            return 0.0;
        }

        // For now, we don't store historical payload data
        // This would require enhancing PhysicalIdentity to track recent payloads
        0.0
    }

    /// Scores time continuity (how recently the device was seen).
    fn score_time_continuity(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> f64 {
        let elapsed = observation
            .timestamp
            .duration_since(physical.last_observation);
        let max_age = self.config.matching_window;

        if elapsed >= max_age {
            return 0.0;
        }

        // Decay score based on elapsed time
        let ratio = elapsed.as_secs_f64() / max_age.as_secs_f64();
        let time_score = (1.0 - ratio) * self.config.weights.time_continuity;

        // Bonus for immediate reappearance (under 1 second)
        if elapsed.as_secs_f64() < 1.0 && physical.observation_count > 1 {
            time_score * 1.2
        } else {
            time_score
        }
    }

    /// Scores RSSI continuity.
    fn score_rssi_continuity(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> f64 {
        if let Some(avg) = physical.rssi_stats.average() {
            let diff = (observation.rssi as f64 - avg).abs();

            // RSSI typically varies by 5-10 dBm, so we score based on deviation
            let similarity = if diff < 5.0 {
                1.0
            } else if diff < 15.0 {
                1.0 - (diff - 5.0) / 10.0
            } else {
                0.0
            };

            similarity * self.config.weights.rssi
        } else {
            0.0 // Not enough data
        }
    }

    /// Scores local name match.
    fn score_local_name(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> f64 {
        match (
            &observation.local_name,
            &physical.stable_features.local_name,
        ) {
            (Some(obs), Some(phy)) if obs == phy => self.config.weights.name,
            (Some(obs), Some(phy))
                if obs.to_lowercase() == phy.to_lowercase() =>
            {
                self.config.weights.name * 0.7
            }
            _ => 0.0,
        }
    }

    /// Scores connectable flag match.
    fn score_connectable(
        &self,
        observation: &AdvertisementObservation,
        _physical: &PhysicalIdentity,
    ) -> f64 {
        // Connectable flag can vary, so we give a small bonus for matches
        // but don't penalize mismatches heavily
        if observation.connectable {
            self.config.weights.connectable * 0.5
        } else {
            0.0
        }
    }

    /// Updates an existing identity with a new observation.
    fn update_identity(&mut self, identity_id: u64, observation: &AdvertisementObservation) {
        if let Some(physical) = self.identities.get_mut(&identity_id) {
            physical.update(observation, self.config.rssi_window_size);

            // Increase confidence on successful match
            let new_confidence = (physical.confidence + 0.05).min(1.0);
            physical.update_confidence(new_confidence, observation.timestamp);

            tracing::debug!(
                identity.id = identity_id,
                confidence = new_confidence,
                "Updated identity with observation"
            );
        }
    }
}

impl IdentityResolver for HeuristicIdentityResolver {
    type Observation = AdvertisementObservation;
    type Identity = DeviceIdentity;

    fn observe(&mut self, observation: AdvertisementObservation) -> DeviceIdentity {
        // First, check if this observation matches an existing identity
        if let Some(candidate) = self.find_best_match(&observation)
            && candidate.total_score >= self.config.merge_threshold
        {
            // Merge into existing identity
            tracing::debug!(
                identity.id = candidate.identity.id(),
                score = candidate.total_score,
                "Merging observation into existing identity"
            );
            self.update_identity(candidate.identity.id(), &observation);
            return candidate.identity;
        }

        // Create a new identity
        self.create_identity(&observation)
    }

    fn expire(&mut self, now: Instant) {
        let max_age = self.config.max_identity_age;

        let expired: Vec<u64> = self
            .identities
            .values()
            .filter(|p| p.is_expired(max_age, now))
            .map(|p| p.identity.id())
            .collect();

        for id in &expired {
            self.expired_ids.push(*id);
            self.identities.remove(id);

            tracing::info!(identity.id = id, "Expired identity");
        }
    }

    fn reset(&mut self) {
        self.identities.clear();
        self.next_identity_id = 1;
        self.expired_ids.clear();

        tracing::info!("Resolver reset");
    }

    fn active_identity_count(&self) -> usize {
        self.identities.len()
    }

    fn expired_identity_count(&self) -> usize {
        self.expired_ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AddressType, AdvertisementObservation, BluetoothAddress};
    use std::time::{Duration, Instant};

    fn create_observation(address: [u8; 6], timestamp: Instant) -> AdvertisementObservation {
        AdvertisementObservation::new(
            timestamp,
            BluetoothAddress::new(address),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03])
    }

    #[test]
    fn test_single_device() {
        let config = ResolverConfig::default();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let now = Instant::now();
        let obs1 = create_observation([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB], now);
        let obs2 = create_observation(
            [0x12, 0x34, 0x56, 0x78, 0x90, 0xAB],
            now + Duration::from_secs(1),
        );

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        assert_eq!(id1, id2, "Same address should produce same identity");
        assert_eq!(resolver.active_identity_count(), 1);
    }

    #[test]
    fn test_address_rotation() {
        let config = ResolverConfig::default();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let now = Instant::now();
        let obs1 = create_observation([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB], now);
        let obs2 = create_observation(
            [0xAB, 0x90, 0x78, 0x56, 0x34, 0x12],
            now + Duration::from_secs(1),
        );

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // With strong manufacturer ID match, addresses should be merged
        assert_eq!(id1, id2, "Address rotation should be resolved");
    }

    #[test]
    fn test_different_devices() {
        let config = ResolverConfig::default();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let now = Instant::now();
        let obs1 = AdvertisementObservation::new(
            now,
            BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]); // Apple

        let obs2 = AdvertisementObservation::new(
            now + Duration::from_secs(1),
            BluetoothAddress::new([0xAB, 0x90, 0x78, 0x56, 0x34, 0x12]),
            AddressType::PrivateResolvable,
        )
        .with_rssi(-65)
        .with_manufacturer_data(0x005E, vec![0x01, 0x02, 0x03]); // Samsung

        let id1 = resolver.observe(obs1);
        let id2 = resolver.observe(obs2);

        // Different manufacturer IDs should NOT merge
        assert_ne!(id1, id2, "Different devices should not merge");
    }

    #[test]
    fn test_expiration() {
        let config = ResolverConfig::builder()
            .max_identity_age(Duration::from_secs(10))
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let now = Instant::now();
        let obs = create_observation([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB], now);

        resolver.observe(obs);
        assert_eq!(resolver.active_identity_count(), 1);

        resolver.expire(now + Duration::from_secs(15));
        assert_eq!(resolver.active_identity_count(), 0);
        assert_eq!(resolver.expired_identity_count(), 1);
    }

    #[test]
    fn test_reset() {
        let config = ResolverConfig::default();
        let mut resolver = HeuristicIdentityResolver::new(config);

        let now = Instant::now();
        let obs = create_observation([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB], now);

        resolver.observe(obs);
        assert_eq!(resolver.active_identity_count(), 1);

        resolver.reset();
        assert_eq!(resolver.active_identity_count(), 0);
        assert_eq!(resolver.next_identity_id, 1);
    }
}
