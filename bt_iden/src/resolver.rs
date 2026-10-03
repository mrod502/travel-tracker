//! Identity resolver trait and implementation.
//!
//! This module defines the core [`IdentityResolver`] trait that all identity
//! resolution engines must implement, along with the concrete
//! [`HeuristicIdentityResolver`] implementation.
//!
//! # Decisions come with their evidence
//!
//! [`IdentityResolver::observe`] answers with an identity, which is what a live
//! caller wants. [`IdentityResolver::resolve`] answers with the same identity plus
//! the [`MatchEvidence`] the decision was made on — the score, the weight that was
//! actually comparable, and the per-feature breakdown. A probabilistic merge of a
//! device that rotates its address on purpose is only reviewable after the fact if
//! the reasoning was kept, so the engine produces it whether or not the caller asks.

use std::collections::HashMap;

use crate::config::ResolverConfig;
use crate::evidence::{Datum, Feature, FeatureSources};
use crate::models::{
    AdvertisementObservation, DeviceIdentity, FeatureScore, MatchEvidence, PhysicalIdentity,
};
use crate::time::ObservationTime;

/// The outcome of one [`IdentityResolver::resolve`] call.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// No existing identity was a candidate; this observation starts a new one.
    New,
    /// Merged into the evidence's identity.
    Merged(MatchEvidence),
    /// A candidate scored at or above the possible threshold but below
    /// [`ResolverConfig::merge_threshold`], so it was not merged.
    BelowThreshold(MatchEvidence),
    /// A candidate cleared the score threshold but the feed covered less of the
    /// designed model than [`ResolverConfig::min_evidence_ratio`], so the inference
    /// was refused. The evidence says what was missing.
    InsufficientEvidence(MatchEvidence),
    /// A candidate scored high enough, but a Direct-quality datum contradicted the
    /// merge — a different manufacturer id or appearance than the identity has
    /// learned. Not merged, and the named feature says why.
    Contradicted(MatchEvidence),
}

impl Outcome {
    /// The evidence behind a decision that considered an existing identity.
    pub fn evidence(&self) -> Option<&MatchEvidence> {
        match self {
            Outcome::New => None,
            Outcome::Merged(e)
            | Outcome::BelowThreshold(e)
            | Outcome::InsufficientEvidence(e)
            | Outcome::Contradicted(e) => Some(e),
        }
    }

    /// Whether the observation was merged into an existing identity.
    pub fn merged(&self) -> bool {
        matches!(self, Outcome::Merged(_))
    }

    /// How much of the designed scoring model the decision could rest on.
    ///
    /// `0.0` for [`Outcome::New`]: nothing was compared, so nothing was covered.
    pub fn coverage(&self) -> f64 {
        self.evidence().map(MatchEvidence::coverage).unwrap_or(0.0)
    }

    /// The feature that refused the merge, for [`Outcome::Contradicted`].
    ///
    /// A report that says only "not merged" is not reviewable: the difference between
    /// "a different manufacturer id" and "a different service list" is the difference
    /// between two devices and one device advertising differently.
    pub fn contradicted_feature(&self) -> Option<&'static str> {
        match self {
            Outcome::Contradicted(e) => vetoed_feature(e),
            _ => None,
        }
    }
}

/// An identity plus the reasoning behind it.
#[derive(Debug, Clone)]
pub struct Resolution {
    /// The identity assigned to the observation.
    pub identity: DeviceIdentity,
    /// How that identity was arrived at.
    pub outcome: Outcome,
}

impl Resolution {
    /// Whether this observation opened a new identity.
    pub fn is_new(&self) -> bool {
        matches!(self.outcome, Outcome::New)
    }
}

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
/// use bt_iden::time::ObservationTime;
///
/// let mut resolver = bt_iden::HeuristicIdentityResolver::new(ResolverConfig::default());
///
/// let obs1 = AdvertisementObservation::new(
///     ObservationTime::now(),
///     BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
///     AddressType::PrivateResolvable,
/// );
///
/// let identity = resolver.observe(obs1);
/// ```
pub trait IdentityResolver {
    /// The type of observation this resolver accepts.
    type Observation;

    /// Records an observation and returns its assigned identity with the reasoning.
    ///
    /// This is the main entry point. The resolver attempts to match the observation
    /// to an existing identity, or opens a new one when no suitable match exists.
    fn resolve(&mut self, observation: Self::Observation) -> Resolution;

    /// Records an observation and returns its assigned identity.
    ///
    /// The [`resolve`] form is preferred wherever the decision will be reviewed,
    /// logged, or written out, because it also reports what the decision was worth.
    ///
    /// [`resolve`]: IdentityResolver::resolve
    fn observe(&mut self, observation: Self::Observation) -> DeviceIdentity {
        self.resolve(observation).identity
    }

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
    fn expire(&mut self, now: ObservationTime);

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
/// # How a candidate is judged
///
/// Four conditions, in this order:
///
/// 1. An exact address match is identification, not inference, and merges.
/// 2. A Direct-quality contradiction (a manufacturer id, appearance or service list
///    that differs from what the identity has learned, or an advertised name that is
///    neither the identity's name nor a truncation of it) refuses the merge — the code
///    that scores "mismatch, strong negative" is not allowed to stop at scoring it.
/// 3. The score has to reach [`ResolverConfig::merge_threshold`].
/// 4. The feed has to have covered [`ResolverConfig::min_evidence_ratio`] of the
///    designed model, so a merge decided on the features one backend happens to
///    expose is distinguishable from one decided on all of them.
///
/// Every candidate that got as far as being scored produces [`MatchEvidence`],
/// whichever way the decision went.
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
/// use bt_iden::time::ObservationTime;
///
/// let config = ResolverConfig::default();
/// let mut resolver = HeuristicIdentityResolver::new(config);
///
/// // Process observations
/// let obs = AdvertisementObservation::new(
///     ObservationTime::now(),
///     BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
///     AddressType::PrivateResolvable,
/// );
/// let identity = resolver.observe(obs);
///
/// // Periodically expire old identities
/// resolver.expire(ObservationTime::now());
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

/// One scorer's answer: how similar things were, when a comparison was possible.
///
/// `None` means the scorer had nothing to compare — the feature is absent from the
/// feed, or the identity has not learned that feature yet. That is different from
/// `Some(0.0)`, which means both sides had a value and the values disagreed: one
/// case leaves the denominator, the other stays in it as negative evidence.
type Comparison = Option<f64>;

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

    /// How confident the resolver currently is in an identity, `0.0` if it holds none.
    ///
    /// An identity that gets persisted has to carry the confidence that was actually
    /// computed. Without this accessor a batch writing identities would have to invent
    /// the number it stores, and a made-up confidence on a merge that a human is being
    /// asked to review is worse than no number.
    pub fn confidence_of(&self, identity: u64) -> f64 {
        self.identities
            .get(&identity)
            .map_or(0.0, |physical| physical.confidence)
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
    fn find_best_match(&self, observation: &AdvertisementObservation) -> Option<MatchEvidence> {
        // A window start that predates any usable clock disables the filter rather
        // than panicking, which is what `Instant - Duration` did here.
        let window_start = observation
            .timestamp
            .checked_sub(self.config.matching_window);

        // Find all active candidates within the matching window
        let candidates: Vec<_> = self
            .identities
            .values()
            .filter(|p| match window_start {
                Some(start) => p.last_observation >= start,
                None => true,
            })
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

    /// Scores an observation against a physical identity, keeping the evidence.
    fn score_observation(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> MatchEvidence {
        let address_matched = self.address_matches(observation, physical);

        let mut features = Vec::with_capacity(Feature::ALL.len());

        // Exact address match (strongest signal, and an identification rather than
        // an inference). It goes in directly rather than through `account`: it earns
        // its points at full strength whatever the provenance around it, and it
        // carries no *weight*, so a merge it decides is still reported as having
        // rested on little of the designed model — which is exactly why the coverage
        // floor exempts it.
        let address_score = if address_matched {
            self.config.weights.manufacturer_id + self.config.weights.time_continuity
        } else {
            0.0
        };
        features.push(FeatureScore {
            feature: Feature::Address.label(),
            score: address_score,
            weight: 0.0,
            datum: if address_matched {
                observation.sources.get(Feature::Address)
            } else {
                Datum::Absent
            },
        });

        features.push(self.score_manufacturer_id(observation, physical));
        features.push(self.score_service_uuids(observation, physical));
        features.push(self.score_appearance(observation, physical));
        features.push(self.score_field_layout(observation, physical));
        features.push(self.score_payload_similarity(observation, physical));
        features.push(self.score_time_continuity(observation, physical));
        features.push(self.score_rssi_continuity(observation, physical));
        features.push(self.score_local_name(observation, physical));
        features.push(self.score_connectable(observation, physical));

        let total_score = features.iter().map(|f| f.score).sum();
        let available_weight = features.iter().map(|f| f.weight).sum();

        let candidate = MatchEvidence {
            identity: physical.identity,
            total_score,
            available_weight,
            designed_weight: FeatureSources::new().designed_weight(&self.config.weights),
            address_matched,
            features,
        };

        if self.config.debug_logging {
            tracing::debug!(
                identity.id = physical.identity.id(),
                score = candidate.total_score,
                coverage = candidate.coverage(),
                features = ?candidate.features.iter()
                    .map(|f| (f.feature, f.score, f.weight))
                    .collect::<Vec<_>>(),
                "Match scoring complete"
            );
        }

        candidate
    }

    /// Turns one scorer's comparison into its accounted contribution.
    ///
    /// This is where the feed's provenance is applied, in one place:
    ///
    /// - a feature with no datum contributes neither score nor weight;
    /// - a feature that could not be compared (the identity has not learned it)
    ///   contributes weight `0.0` so it cannot depress a candidate that simply has
    ///   nothing to match against;
    /// - a comparison made on derived data earns [`ResolverConfig::derived_evidence_factor`]
    ///   of what it would have earned from the real datum, and its pair quality is
    ///   the weaker of the two sides.
    fn account(
        &self,
        feature: Feature,
        comparison: Comparison,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let observed = observation.sources.get(feature);
        let learned = physical.stable_features.datums.get(feature);
        let quality = observed.min(learned);

        let datum = match (observed, learned) {
            // Reporting a quality for a feature that never ran would make the
            // absence invisible.
            _ if comparison.is_none() || observed == Datum::Absent => Datum::Absent,
            _ => quality,
        };

        let Some(similarity) = comparison.filter(|_| observed.is_present()) else {
            return FeatureScore {
                feature: feature.label(),
                score: 0.0,
                weight: 0.0,
                datum,
            };
        };

        let weight = feature.weight(&self.config.weights);
        let factor = if quality == Datum::Derived {
            self.config.derived_evidence_factor
        } else {
            1.0
        };

        FeatureScore {
            feature: feature.label(),
            score: similarity * weight * factor,
            weight,
            datum,
        }
    }

    /// Whether the observation's address is one this identity already answers to.
    ///
    /// The whole address history counts, not just the current address: a device
    /// that rotates forward and back to an earlier address is still the same
    /// device, and an identity that forgot its own history would split it.
    fn address_matches(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> bool {
        physical.answering_addresses().any(|address| {
            *address == observation.address
                && observation.sources.get(Feature::Address).is_present()
        })
    }

    /// The learned value for a feature, paired with the observation's value, when
    /// both sides have one.
    fn both_present<T>(
        &self,
        feature: Feature,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
        learned: impl Fn(&PhysicalIdentity) -> Option<T>,
        observed: impl Fn(&AdvertisementObservation) -> Option<T>,
    ) -> Option<(T, T)> {
        if observation.sources.get(feature) == Datum::Absent {
            return None;
        }
        let learned = learned(physical)?;
        let observed = observed(observation)?;
        Some((learned, observed))
    }

    /// Scores manufacturer ID match.
    fn score_manufacturer_id(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let comparison = self
            .both_present(
                Feature::ManufacturerId,
                observation,
                physical,
                |p| p.stable_features.manufacturer_id,
                |o| o.manufacturer_id,
            )
            .map(|(learned, observed)| if learned == observed { 1.0 } else { 0.0 });

        self.account(Feature::ManufacturerId, comparison, observation, physical)
    }

    /// Scores service UUID overlap.
    fn score_service_uuids(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let comparison = self
            .both_present(
                Feature::ServiceUuids,
                observation,
                physical,
                |p| {
                    (!p.stable_features.service_uuids.is_empty())
                        .then(|| p.stable_features.service_uuids.clone())
                },
                |o| (!o.service_uuids.is_empty()).then(|| o.service_uuids.clone()),
            )
            .map(|(learned, observed)| jaccard(&learned, &observed));

        self.account(Feature::ServiceUuids, comparison, observation, physical)
    }

    /// Scores appearance match.
    fn score_appearance(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let comparison = self
            .both_present(
                Feature::Appearance,
                observation,
                physical,
                |p| p.stable_features.appearance,
                |o| o.appearance,
            )
            .map(|(learned, observed)| if learned == observed { 1.0 } else { 0.0 });

        self.account(Feature::Appearance, comparison, observation, physical)
    }

    /// Scores AD field layout similarity.
    ///
    /// Compares the set of AD type codes against every layout this identity has
    /// recently shown, taking the best match: a device alternates between
    /// advertisement forms (with and without a name, say) and the identity that has
    /// seen both should recognise either.
    ///
    /// Only a layout that was actually parsed out of an advertisement counts. The
    /// fallback in [`AdvertisementObservation::effective_layout`] guesses the set from
    /// whichever fields the feed happened to fill in, which is entailed by those same
    /// fields — scoring it would charge the manufacturer id or the name a second time
    /// under a different label, and would hand every observation from the same
    /// lossy backend a free 15-point agreement with every other. Abstaining leaves the
    /// feature out of the denominator instead, so a feed that never sees structures is
    /// simply scored on what it can supply.
    fn score_field_layout(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let (layout, datum) = observation.effective_layout();
        if datum != Datum::Direct || layout.is_empty() {
            return FeatureScore {
                feature: Feature::FieldLayout.label(),
                score: 0.0,
                weight: 0.0,
                datum: Datum::Absent,
            };
        }

        let comparison = (!physical.stable_features.field_layouts.is_empty()).then(|| {
            physical
                .stable_features
                .field_layouts
                .iter()
                .map(|learned| jaccard(learned, &layout))
                .fold(0.0f64, f64::max)
        });

        self.account(Feature::FieldLayout, comparison, observation, physical)
    }

    /// Scores manufacturer payload similarity.
    ///
    /// Similarity is the fraction of the longer payload covered by the shared
    /// prefix. The rotating part of these payloads — Apple's and Google's counters
    /// and proximity values — sits at the end, so a long shared prefix is the
    /// stable part of the advertisement and a mismatch deep into the payload is
    /// real disagreement.
    fn score_payload_similarity(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let (payload, datum) = observation.effective_payload();
        if datum == Datum::Absent || payload.is_empty() {
            return FeatureScore {
                feature: Feature::PayloadSimilarity.label(),
                score: 0.0,
                weight: 0.0,
                datum: Datum::Absent,
            };
        }

        let comparison = (!physical.stable_features.payloads.is_empty()).then(|| {
            physical
                .stable_features
                .payloads
                .iter()
                .map(|learned| common_prefix_ratio(learned, &payload))
                .fold(0.0f64, f64::max)
        });

        self.account(
            Feature::PayloadSimilarity,
            comparison,
            observation,
            physical,
        )
    }

    /// Scores time continuity (how recently the device was seen).
    fn score_time_continuity(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        // An observation older than the identity's latest one arrives out of order.
        // "0 seconds ago" would be the most flattering reading of it and the least
        // honest, so the feature abstains and leaves the denominator.
        if observation
            .timestamp
            .ordering(&physical.last_observation)
            .is_lt()
        {
            return FeatureScore {
                feature: Feature::TimeContinuity.label(),
                score: 0.0,
                weight: 0.0,
                datum: Datum::Absent,
            };
        }

        let elapsed = observation
            .timestamp
            .elapsed_since(&physical.last_observation);
        let max_age = self.config.matching_window;

        let comparison = if elapsed >= max_age {
            Some(0.0)
        } else {
            let ratio = elapsed.as_secs_f64() / max_age.as_secs_f64();
            let time_score = 1.0 - ratio;

            // Bonus for immediate reappearance (under 1 second)
            if elapsed.as_secs_f64() < 1.0 && physical.observation_count > 1 {
                Some((time_score * 1.2).min(1.0))
            } else {
                Some(time_score)
            }
        };

        self.account(Feature::TimeContinuity, comparison, observation, physical)
    }

    /// Scores RSSI continuity.
    fn score_rssi_continuity(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let comparison = physical
            .rssi_stats
            .average()
            .map(|avg| (observation.rssi as f64 - avg).abs())
            .map(|diff| {
                // RSSI typically varies by 5-10 dBm, so we score based on deviation
                if diff < 5.0 {
                    1.0
                } else if diff < 15.0 {
                    1.0 - (diff - 5.0) / 10.0
                } else {
                    0.0
                }
            });

        self.account(Feature::Rssi, comparison, observation, physical)
    }

    /// Scores local name match.
    ///
    /// A name that is neither equal nor a truncation of the other scores 0, which under
    /// [`vetoed_feature`] is a refusal rather than a shortfall: two devices that advertise
    /// different names are two devices until something says otherwise.
    fn score_local_name(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let comparison = self
            .both_present(
                Feature::Name,
                observation,
                physical,
                |p| p.stable_features.local_name.clone(),
                |o| o.local_name.clone(),
            )
            .map(|(learned, observed)| {
                if learned == observed {
                    1.0
                } else if learned.to_lowercase() == observed.to_lowercase() {
                    0.7
                } else if truncation_of(&learned, &observed) {
                    0.5
                } else {
                    0.0
                }
            });

        self.account(Feature::Name, comparison, observation, physical)
    }

    /// Scores the connectable flag.
    ///
    /// This compares the flag rather than rewarding a device for being connectable:
    /// a rule that hands out points to every connectable advertisement would push
    /// unrelated connectable devices together, which is the opposite of what a
    /// feature worth 5 points is for.
    fn score_connectable(
        &self,
        observation: &AdvertisementObservation,
        physical: &PhysicalIdentity,
    ) -> FeatureScore {
        let comparison = self
            .both_present(
                Feature::Connectable,
                observation,
                physical,
                |p| p.stable_features.connectable,
                |o| Some(o.connectable),
            )
            .map(|(learned, observed)| if learned == observed { 1.0 } else { 0.0 });

        self.account(Feature::Connectable, comparison, observation, physical)
    }

    /// A Direct-quality datum the identity already knows, that this observation
    /// flatly contradicts.
    ///
    /// Delegates to [`vetoed_feature`]; see that function for which features veto and
    /// why an exact address match outranks them.
    fn contradiction(&self, candidate: &MatchEvidence) -> Option<&'static str> {
        vetoed_feature(candidate)
    }

    /// Decides what to do with the best candidate.
    fn decide(
        &mut self,
        observation: &AdvertisementObservation,
        candidate: MatchEvidence,
    ) -> Outcome {
        if let Some(feature) = self.contradiction(&candidate) {
            tracing::debug!(
                identity.id = candidate.identity.id(),
                score = candidate.total_score,
                feature,
                "Merge refused: a direct datum contradicts the identity"
            );
            return Outcome::Contradicted(candidate);
        }

        if candidate.total_score < self.config.merge_threshold {
            return Outcome::BelowThreshold(candidate);
        }

        if !candidate.address_matched && candidate.coverage() < self.config.min_evidence_ratio {
            tracing::debug!(
                identity.id = candidate.identity.id(),
                score = candidate.total_score,
                coverage = candidate.coverage(),
                "Merge refused: too little of the scoring model was available"
            );
            return Outcome::InsufficientEvidence(candidate);
        }

        tracing::debug!(
            identity.id = candidate.identity.id(),
            score = candidate.total_score,
            coverage = candidate.coverage(),
            "Merging observation into existing identity"
        );
        self.update_identity(candidate.identity.id(), observation);
        Outcome::Merged(candidate)
    }

    /// Records an observation into an existing identity and bumps its confidence.
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

    fn resolve(&mut self, observation: AdvertisementObservation) -> Resolution {
        let Some(candidate) = self.find_best_match(&observation) else {
            let identity = self.create_identity(&observation);
            return Resolution {
                identity,
                outcome: Outcome::New,
            };
        };

        let considered = candidate.identity;
        let outcome = self.decide(&observation, candidate);

        // A candidate that was considered and refused does not carry the observation:
        // the observation opens its own identity, which is what keeps the refused
        // evidence attached to the observation it describes.
        let identity = if outcome.merged() {
            considered
        } else {
            self.create_identity(&observation)
        };

        Resolution { identity, outcome }
    }

    fn expire(&mut self, now: ObservationTime) {
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

/// A Direct-quality datum the identity already knows, that this observation flatly
/// contradicts.
///
/// Only [`Datum::Direct`] conflicts count: a derived value disagreeing is worth what
/// derived evidence is worth — a note, not a veto — and a device whose appearance or
/// manufacturer id genuinely changed is handled by
/// [`LearnedFeatures::merge`](crate::models::LearnedFeatures::merge) clearing the
/// learned value rather than by a permanent unexplained zero here.
///
/// The four vetoing features are the ones where disagreement is evidence about the
/// *device* rather than about one advertisement: a different manufacturer id, a
/// different appearance, two service-UUID lists with nothing in common, or two advertised
/// names where neither is a truncation of the other. Every other feature scores 0 merely
/// by being unrelated, which is why 0 there is not a veto. Without this rule, two devices
/// sharing nothing but a signal level and a matching advertisement shape reach the merge
/// threshold on timing alone — and so does a fleet of identically-provisioned beacons,
/// which is what puts the name in this list: same manufacturer id, same AD layout,
/// near-identical payload, names differing only in a trailing digit. The name earns the
/// veto because advertising a different name is a statement the device itself makes, while
/// payload and layout are things a whole product line shares.
///
/// Being seen at an address the identity already answers to outranks a conflict: one
/// address cannot be two devices, so the conflicting datum — not the identity — is the
/// thing in question.
fn vetoed_feature(candidate: &MatchEvidence) -> Option<&'static str> {
    if candidate.address_matched {
        return None;
    }
    candidate
        .features
        .iter()
        .find(|f| {
            f.weight > 0.0
                && f.score == 0.0
                && f.datum == Datum::Direct
                && matches!(
                    f.feature,
                    "manufacturer_id" | "appearance" | "uuid_overlap" | "name"
                )
        })
        .map(|f| f.feature)
}

/// True when either name is a leading substring of the other, ignoring case.
///
/// A legacy advertisement carries about 30 bytes of name, and a stack that also knows the
/// GATT name reports that one, so `Pixel 7 P` beside `Pixel 7 Pro` is the same name cut
/// short rather than a rename — and must not be read as a device-level disagreement. Two
/// names that differ somewhere other than at the end of their shared prefix, like
/// `Mock Beacon 0` beside `Mock Beacon 1`, are a different name.
fn truncation_of(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    a != b && (a.starts_with(&b) || b.starts_with(&a))
}

/// Similarity between two collections, as intersection over union.
fn jaccard<T: Eq + std::hash::Hash>(a: &[T], b: &[T]) -> f64 {
    use std::collections::HashSet;

    let left: HashSet<&T> = a.iter().collect();
    let right: HashSet<&T> = b.iter().collect();
    let union = left.union(&right).count();
    if union == 0 {
        return 0.0;
    }
    left.intersection(&right).count() as f64 / union as f64
}

/// Fraction of the longer slice covered by the shared leading bytes.
fn common_prefix_ratio(a: &[u8], b: &[u8]) -> f64 {
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    common as f64 / longest as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ad::AdStructures;
    use crate::models::{AddressType, BluetoothAddress};
    use std::time::{Duration, UNIX_EPOCH};

    /// A fixed wall clock, so a test's intervals are the ones it says they are.
    fn wall(secs: u64) -> ObservationTime {
        ObservationTime::from_wall(UNIX_EPOCH + Duration::from_secs(secs))
    }

    const BASE: u64 = 1_700_000_000;

    fn address(byte: u8) -> BluetoothAddress {
        BluetoothAddress::new([0x00, 0x00, 0x00, 0x00, 0x00, byte])
    }

    fn bare(timestamp: ObservationTime, byte: u8) -> AdvertisementObservation {
        AdvertisementObservation::new(timestamp, address(byte), AddressType::PrivateResolvable)
    }

    /// A realistically thin observation: the three features a decoded-GATT backend
    /// can actually supply, worth 70 of the designed 185 points plus timing.
    fn thin(timestamp: ObservationTime, byte: u8) -> AdvertisementObservation {
        bare(timestamp, byte)
            .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03])
            .with_rssi(-60)
    }

    fn feature<'a>(evidence: &'a MatchEvidence, name: &str) -> &'a FeatureScore {
        evidence
            .features
            .iter()
            .find(|f| f.feature == name)
            .unwrap_or_else(|| panic!("no {name} in {:?}", evidence.features))
    }

    #[test]
    fn a_pair_of_stored_rows_resolves_like_the_live_capture_that_wrote_them() {
        // The point of the wall-clock half of `ObservationTime`: rows written last
        // month carry no monotonic reading, and re-resolving them must not depend on
        // when the process doing it happens to have started.
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());

        let founded = resolver.resolve(thin(wall(BASE), 0xAA));
        let again = resolver.resolve(thin(wall(BASE + 1), 0xBB));

        assert!(founded.is_new());
        assert!(
            again.outcome.merged(),
            "a replayed pair one second apart should merge, got {:?}",
            again.outcome
        );
        assert_eq!(founded.identity, again.identity);
        assert_eq!(
            feature(again.outcome.evidence().unwrap(), "time_continuity").weight,
            Feature::TimeContinuity.weight(&ResolverConfig::default().weights),
            "the missing monotonic clock must not make the feature abstain"
        );
    }

    #[test]
    fn an_observation_older_than_the_identity_abstains_from_time_continuity() {
        // Rows come back in whatever order they were read. Reading "0 seconds ago"
        // out of that would be the most flattering possible interpretation of a row
        // that arrived out of order, so the feature declines to score it.
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        resolver.resolve(thin(wall(BASE + 10), 0xAA));

        let late = resolver.resolve(thin(wall(BASE), 0xBB));
        let time = feature(late.outcome.evidence().unwrap(), "time_continuity");
        assert_eq!(time.score, 0.0);
        assert_eq!(time.weight, 0.0);
        assert_eq!(time.datum, Datum::Absent);
    }

    #[test]
    fn a_feature_the_feed_cannot_supply_leaves_the_denominator() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        resolver.resolve(thin(wall(BASE), 0xAA));

        let evidence = resolver
            .resolve(thin(wall(BASE + 1), 0xBB))
            .outcome
            .evidence()
            .expect("a merge carries its evidence")
            .clone();

        for name in [
            Feature::Appearance.label(),
            Feature::Name.label(),
            Feature::ServiceUuids.label(),
            Feature::FieldLayout.label(),
            Feature::Connectable.label(),
        ] {
            let score = feature(&evidence, name);
            assert_eq!(score.weight, 0.0, "{name} should not be in the denominator");
            assert_eq!(score.datum, Datum::Absent, "{name} was never supplied");
        }

        // 40 manufacturer + 20 payload + 10 rssi + 25 time, out of 185 designed.
        assert_eq!(evidence.available_weight, 95.0);
        assert_eq!(evidence.designed_weight, 185.0);
        assert!(evidence.coverage() > 0.51 && evidence.coverage() < 0.52);
    }

    #[test]
    fn derived_evidence_earns_half_and_does_not_change_coverage() {
        let score_with = |datum: Datum| {
            let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
            let build = |byte, at| {
                bare(at, byte).with_manufacturer_provenance(
                    Some(0x004C),
                    vec![0x01, 0x02],
                    datum,
                    datum,
                )
            };
            resolver.resolve(build(0xAA, wall(BASE)));
            resolver
                .resolve(build(0xBB, wall(BASE + 1)))
                .outcome
                .evidence()
                .expect("both runs consider the identity")
                .clone()
        };

        let direct = score_with(Datum::Direct);
        let derived = score_with(Datum::Derived);

        assert_eq!(feature(&direct, "manufacturer_id").score, 40.0);
        assert_eq!(feature(&derived, "manufacturer_id").score, 20.0);
        assert_eq!(feature(&derived, "manufacturer_id").datum, Datum::Derived);
        // The proxy data was still supplied, so it still counts as coverage: what it
        // bought is a smaller number of points, not a smaller denominator.
        assert_eq!(direct.available_weight, derived.available_weight);
        assert_eq!(direct.coverage(), derived.coverage());
        assert!(derived.total_score < direct.total_score);
    }

    #[test]
    fn an_exact_address_match_merges_below_the_evidence_floor() {
        // An address the identity already answers to identifies the device rather
        // than inferring it, so a feed that can supply nothing else still merges -
        // and the coverage number says honestly how little was compared.
        let config = ResolverConfig::default();
        let mut resolver = HeuristicIdentityResolver::new(config.clone());
        resolver.resolve(bare(wall(BASE), 0xAA));

        let again = resolver.resolve(bare(wall(BASE + 1), 0xAA));
        assert!(again.outcome.merged());
        assert!(again.outcome.evidence().unwrap().address_matched);
        assert!(
            again.outcome.coverage() < config.min_evidence_ratio,
            "this test is only about the exemption if the coverage is under the floor"
        );
        assert_eq!(
            feature(again.outcome.evidence().unwrap(), "address_match").weight,
            0.0
        );
    }

    #[test]
    fn a_direct_conflict_refuses_the_merge_and_names_the_feature() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let apple = resolver.resolve(thin(wall(BASE), 0xAA));

        let samsung = resolver.resolve(
            bare(wall(BASE + 1), 0xBB)
                .with_manufacturer_data(0x005E, vec![0x01, 0x02, 0x03])
                .with_rssi(-60),
        );

        assert_ne!(
            apple.identity, samsung.identity,
            "a different manufacturer id is a different device"
        );
        assert!(
            matches!(samsung.outcome, Outcome::Contradicted(_)),
            "got {:?}",
            samsung.outcome
        );
        assert_eq!(
            samsung.outcome.contradicted_feature(),
            Some("manufacturer_id"),
            "the refusal has to say which datum disagreed"
        );
    }

    #[test]
    fn disjoint_service_lists_are_a_conflict_too() {
        let uuid = |n: u128| uuid::Uuid::from_u128(n);
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let first = |byte, at, listed: u128| {
            bare(at, byte)
                .with_service_uuids(vec![uuid(listed)], Datum::Direct)
                .with_rssi(-60)
        };

        resolver.resolve(first(0xAA, wall(BASE), 0x180D));
        let other = resolver.resolve(first(0xBB, wall(BASE + 1), 0x180F));

        assert_eq!(
            other.outcome.contradicted_feature(),
            Some("uuid_overlap"),
            "two Direct service lists with nothing in common is evidence about the device"
        );
    }

    #[test]
    fn a_different_advertised_name_is_a_different_device_even_on_identical_payloads() {
        // The fleet case. A product line ships the same manufacturer id, the same AD
        // layout and a payload that differs in a couple of bytes, and the units are told
        // apart by a trailing digit of the name — so every weighted feature agrees and the
        // candidate sails past the merge threshold. Advertising a name is a statement the
        // device makes about itself, unlike a layout a whole product line shares, which is
        // why the name vetoes where the others merely score.
        let config = ResolverConfig::default();
        let mut resolver = HeuristicIdentityResolver::new(config.clone());
        let beacon = |byte: u8, at: u64, name: &str| {
            bare(wall(at), byte)
                .with_manufacturer_data(0xFFFF, vec![0xFF, 0x01, 0x02])
                .with_field_layout(vec![0x01, 0x09, 0xFF])
                .with_local_name_datum(name.to_string(), Datum::Direct)
                .with_rssi(-60)
        };

        resolver.resolve(beacon(0xAA, BASE, "Mock Beacon 0"));
        let second = resolver.resolve(beacon(0xBB, BASE + 1, "Mock Beacon 1"));

        assert!(
            second.outcome.evidence().unwrap().total_score >= config.merge_threshold,
            "this test is only about the veto if the score would otherwise have merged: {}",
            second.outcome.evidence().unwrap().total_score
        );
        assert!(
            matches!(second.outcome, Outcome::Contradicted(_)),
            "got {:?}",
            second.outcome
        );
        assert_eq!(
            second.outcome.contradicted_feature(),
            Some("name"),
            "the refusal has to say that the name is what disagreed"
        );
    }

    #[test]
    fn a_truncated_name_is_the_same_name_and_not_a_conflict() {
        // A legacy advertisement carries about 30 bytes of name and a stack that knows the
        // GATT name reports that one, so the same device reaches the resolver under both
        // spellings. Prefix tolerance is what keeps the veto from splitting them.
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let phone = |byte: u8, at: u64, name: &str| {
            bare(wall(at), byte)
                .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03])
                .with_local_name_datum(name.to_string(), Datum::Direct)
                .with_rssi(-60)
        };

        resolver.resolve(phone(0xAA, BASE, "Pixel 7 Pro"));
        let cut_short = resolver.resolve(phone(0xBB, BASE + 1, "Pixel 7 P"));

        assert_eq!(
            cut_short.outcome.contradicted_feature(),
            None,
            "a name cut short is not a device-level disagreement"
        );
        assert!(
            cut_short.outcome.merged(),
            "got {:?} for a name that is a prefix of the learned one",
            cut_short.outcome
        );
    }

    #[test]
    fn a_renamed_device_at_the_same_address_is_still_one_device() {
        // One address cannot be two devices, so the name is the datum in question rather
        // than the identity — the same precedence that outranks a conflicting manufacturer
        // id, now covering the veto this feature adds.
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let founded = resolver.resolve(
            thin(wall(BASE), 0xAA)
                .with_local_name_datum("Living Room Speaker".to_string(), Datum::Direct),
        );

        let renamed = resolver.resolve(
            thin(wall(BASE + 1), 0xAA)
                .with_local_name_datum("Kitchen Speaker".to_string(), Datum::Direct),
        );

        assert_eq!(renamed.identity, founded.identity);
        assert!(renamed.outcome.merged());
        assert_eq!(renamed.outcome.contradicted_feature(), None);
    }

    #[test]
    fn a_name_the_other_observation_never_advertised_is_not_a_conflict() {
        // The veto needs two names. A device that advertises a name sometimes and not
        // others is the normal case, and absence has to stay absence rather than become
        // evidence of a different device.
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        resolver.resolve(
            thin(wall(BASE), 0xAA)
                .with_local_name_datum("Living Room Speaker".to_string(), Datum::Direct),
        );

        let nameless = resolver.resolve(thin(wall(BASE + 1), 0xBB));

        assert_eq!(
            nameless.outcome.contradicted_feature(),
            None,
            "no name on one side is a missing feature, not a disagreement"
        );
        assert!(nameless.outcome.merged());
    }

    #[test]
    fn an_address_the_identity_answers_to_outranks_a_conflicting_feature() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let founded = resolver.resolve(thin(wall(BASE), 0xAA));

        // Same address, different manufacturer id: the datum is the thing in doubt.
        let again = resolver.resolve(
            bare(wall(BASE + 1), 0xAA)
                .with_manufacturer_data(0x005E, vec![0x01, 0x02, 0x03])
                .with_rssi(-60),
        );

        assert_eq!(founded.identity, again.identity);
        assert!(again.outcome.merged());
        assert_eq!(again.outcome.contradicted_feature(), None);
    }

    #[test]
    fn an_identity_recognises_an_address_it_rotated_back_to() {
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        resolver.resolve(thin(wall(BASE), 0xAA));
        resolver.resolve(thin(wall(BASE + 1), 0xBB));

        let back = resolver.resolve(thin(wall(BASE + 2), 0xAA));
        assert!(
            back.outcome.evidence().unwrap().address_matched,
            "a device that rotates forward and back is still the same device"
        );
        assert_eq!(resolver.active_identity_count(), 1);
    }

    #[test]
    fn a_full_advertisement_covers_almost_the_whole_model() {
        let mut bytes = vec![
            0x02, 0x01, 0x06, // Flags
            0x06, 0x09, b'H', b'R', b'M', b'-', b'1', // Complete local name
            0x03, 0x19, 0x40, 0x03, // Appearance 0x0340
            0x05, 0xFF, 0x4C, 0x00, 0x12, 0x34, // Manufacturer 0x004C, payload 12 34
        ];
        bytes.extend_from_slice(&[0x11, 0x07]); // 128-bit service UUIDs
        bytes.extend_from_slice(&(0xA0u8..=0xAF).collect::<Vec<u8>>());

        let ad = AdStructures::parse(&bytes);
        let built = |at, byte| bare(at, byte).with_ad_structures(&ad).with_rssi(-58);

        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let founded = resolver.resolve(built(wall(BASE), 0xAA));
        let rotated = resolver.resolve(built(wall(BASE + 1), 0xBB));

        let evidence = rotated
            .outcome
            .evidence()
            .expect("a merge keeps its evidence");
        assert!(founded.is_new());
        assert!(
            rotated.outcome.merged(),
            "got {:?} at coverage {}",
            rotated.outcome,
            evidence.coverage()
        );
        // Everything but connectability, which is stated in the PDU type and not in
        // any advertisement, and so can only ever be inferred from the flags byte.
        assert!(
            evidence.coverage() > 0.8,
            "coverage was {}",
            evidence.coverage()
        );
        assert_eq!(feature(evidence, "field_layout").datum, Datum::Direct);
        assert_eq!(feature(evidence, "connectable").datum, Datum::Derived);
        assert_eq!(evidence.top_features(1)[0].0, "manufacturer_id");
    }

    #[test]
    fn a_synthesized_layout_is_not_scored_as_a_structure() {
        // A layout guessed from the fields a feed happens to carry is those same
        // fields restated; scoring it would charge them twice and hand every
        // observation from a lossy backend a free agreement with every other.
        let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
        let name_only = |at, byte| bare(at, byte).with_local_name("TAG-01".to_string());

        resolver.resolve(name_only(wall(BASE), 0xAA));
        let again = resolver.resolve(name_only(wall(BASE + 1), 0xBB));

        let layout = feature(again.outcome.evidence().unwrap(), "field_layout");
        assert_eq!(layout.weight, 0.0);
        assert_eq!(layout.datum, Datum::Absent);
    }

    #[test]
    fn identities_expire_on_stored_timestamps() {
        let config = ResolverConfig::builder()
            .max_identity_age(Duration::from_secs(30))
            .build();
        let mut resolver = HeuristicIdentityResolver::new(config);
        resolver.resolve(thin(wall(BASE), 0xAA));

        assert_eq!(resolver.active_identity_count(), 1);
        // `expire` takes the same type an observation carries, so a replay can expire
        // its own past rather than only its own present.
        resolver.expire(wall(BASE + 31));
        assert_eq!(resolver.active_identity_count(), 0);
        assert_eq!(resolver.expired_identity_count(), 1);
    }
}
