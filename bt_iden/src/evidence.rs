//! Evidence accounting — how much of the scoring model a feed can actually speak.
//!
//! The resolver adds ten weighted features together and compares the sum against
//! a fixed threshold. That comparison only means what it is supposed to mean if
//! every feature had something to say, and real feeds do not look like that:
//!
//! - A backend that hands over decoded GATT properties never sees an AD
//!   structure, so `field_layout` and `appearance` have no source at all.
//! - A stored occurrence kept only the *keys* of its service-data map, so the
//!   value scored as `service_uuids` is a proxy for the advertised service list
//!   rather than that list.
//! - A capture with no name advertised cannot earn the name feature either way.
//!
//! Without accounting, all three cases look like *evidence of difference*: the
//! missing feature contributes nothing to the score while still sitting in the
//! denominator, so a degraded feed quietly raises the bar until merges stop
//! happening and nothing reports why. With accounting, each feature declares
//! where its value came from, and a merge decision carries the fraction of the
//! designed model that was available to make it.
//!
//! # Example
//!
//! ```
//! use bt_iden::evidence::{Datum, Feature, FeatureSources};
//! use bt_iden::models::ScoringWeights;
//!
//! let weights = ScoringWeights::default();
//!
//! // A feed that only ever reports an address and a signal level.
//! let thin = FeatureSources::new()
//!     .with(Feature::Address, Datum::Direct)
//!     .with(Feature::Rssi, Datum::Direct);
//! let coverage = thin.coverage(&weights);
//! assert!(coverage.ratio() < 0.25);
//! assert_eq!(thin.missing(&weights)[0].0, Feature::ManufacturerId);
//!
//! // The designed model, fully fed, covers everything.
//! assert_eq!(FeatureSources::all_direct().coverage(&weights).ratio(), 1.0);
//! ```

use crate::models::ScoringWeights;

/// How a feature's value relates to the datum its scorer was designed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Datum {
    /// The feed has no source for this feature. Contributes neither score nor
    /// weight: it is not evidence of anything.
    #[default]
    Absent,
    /// The value stands in for the designed datum — the advertised service list
    /// read off service-data keys, a name copied from a cache, connectability
    /// inferred from the flags byte. It is worth something, and it is worth less
    /// than the real thing, because the thing it stands in for can change without
    /// the designed datum changing.
    Derived,
    /// The value is the datum the scorer was designed for.
    Direct,
}

impl Datum {
    /// `true` when there is something to compare.
    pub fn is_present(self) -> bool {
        !matches!(self, Datum::Absent)
    }
}

/// A scored feature of the identity model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Exact address match, the decisive case that needs no inference.
    Address,
    /// The 16-bit manufacturer (company) identifier.
    ManufacturerId,
    /// The advertised service UUID list.
    ServiceUuids,
    /// The advertised local name.
    Name,
    /// The ordering and set of AD structures in the advertisement.
    FieldLayout,
    /// The device appearance value.
    Appearance,
    /// Byte-level similarity of the advertisement payload.
    PayloadSimilarity,
    /// Recency of the previous observation of this identity.
    TimeContinuity,
    /// Continuity of the signal level.
    Rssi,
    /// Whether the device advertises as connectable.
    Connectable,
}

impl Feature {
    /// Every feature, in declaration order.
    pub const ALL: [Feature; 10] = [
        Feature::Address,
        Feature::ManufacturerId,
        Feature::ServiceUuids,
        Feature::Name,
        Feature::FieldLayout,
        Feature::Appearance,
        Feature::PayloadSimilarity,
        Feature::TimeContinuity,
        Feature::Rssi,
        Feature::Connectable,
    ];

    /// Index into a [`FeatureSources`] backing array.
    pub(crate) fn index(self) -> usize {
        match self {
            Feature::Address => 0,
            Feature::ManufacturerId => 1,
            Feature::ServiceUuids => 2,
            Feature::Name => 3,
            Feature::FieldLayout => 4,
            Feature::Appearance => 5,
            Feature::PayloadSimilarity => 6,
            Feature::TimeContinuity => 7,
            Feature::Rssi => 8,
            Feature::Connectable => 9,
        }
    }

    /// Stable name for logs and reports; matches the keys in the resolver's
    /// component map.
    pub fn label(self) -> &'static str {
        match self {
            Feature::Address => "address_match",
            Feature::ManufacturerId => "manufacturer_id",
            Feature::ServiceUuids => "uuid_overlap",
            Feature::Name => "name",
            Feature::FieldLayout => "field_layout",
            Feature::Appearance => "appearance",
            Feature::PayloadSimilarity => "payload_similarity",
            Feature::TimeContinuity => "time_continuity",
            Feature::Rssi => "rssi",
            Feature::Connectable => "connectable",
        }
    }

    /// The weight this feature carries in `weights`.
    ///
    /// [`Feature::Address`] has no weight of its own: an exact address match
    /// earns the manufacturer and time weights together, so its own weight
    /// reports `0.0` and is deliberately excluded from the coverage denominator
    /// (see [`FeatureSources::designed_weight`]).
    pub fn weight(self, weights: &ScoringWeights) -> f64 {
        match self {
            Feature::Address => 0.0,
            Feature::ManufacturerId => weights.manufacturer_id,
            Feature::ServiceUuids => weights.uuid_overlap,
            Feature::Name => weights.name,
            Feature::FieldLayout => weights.field_layout,
            Feature::Appearance => weights.appearance,
            Feature::PayloadSimilarity => weights.payload_similarity,
            Feature::TimeContinuity => weights.time_continuity,
            Feature::Rssi => weights.rssi,
            Feature::Connectable => weights.connectable,
        }
    }
}

/// Which datum each feature's value came from, for one feed or one observation.
#[derive(Debug, Clone, Copy)]
pub struct FeatureSources {
    entries: [Datum; 10],
}

impl Default for FeatureSources {
    /// Nothing known — the fail-closed starting point, so a feed that never
    /// declares its provenance earns no weight anywhere rather than being trusted.
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureSources {
    /// All features absent.
    pub fn new() -> Self {
        Self {
            entries: [Datum::Absent; 10],
        }
    }

    /// Every feature backed by its designed datum — the reference case the
    /// thresholds were written against.
    pub fn all_direct() -> Self {
        Self {
            entries: [Datum::Direct; 10],
        }
    }

    /// The datum available for one feature.
    pub fn get(&self, feature: Feature) -> Datum {
        self.entries[feature.index()]
    }

    /// Sets one feature's datum, for builders that know their own provenance.
    pub fn set(&mut self, feature: Feature, datum: Datum) {
        self.entries[feature.index()] = datum;
    }

    /// Builder form of [`FeatureSources::set`].
    #[must_use]
    pub fn with(mut self, feature: Feature, datum: Datum) -> Self {
        self.set(feature, datum);
        self
    }

    /// Features with something to compare.
    pub fn present(&self) -> Vec<Feature> {
        Feature::ALL
            .into_iter()
            .filter(|f| self.get(*f).is_present())
            .collect()
    }

    /// Weight of every feature that has any value at all. This is the ceiling the
    /// score could reach, before quality discounts.
    pub fn available_weight(&self, weights: &ScoringWeights) -> f64 {
        Feature::ALL
            .into_iter()
            .filter(|f| self.get(*f).is_present())
            .map(|f| f.weight(weights))
            .sum()
    }

    /// Total weight of the designed model, excluding the address bypass, which is
    /// decisive evidence rather than a scored feature.
    pub fn designed_weight(&self, weights: &ScoringWeights) -> f64 {
        Feature::ALL
            .into_iter()
            .filter(|f| *f != Feature::Address)
            .map(|f| f.weight(weights))
            .sum()
    }

    /// Designed features with no source, heaviest first — the answer to "what is
    /// this feed costing me".
    pub fn missing(&self, weights: &ScoringWeights) -> Vec<(Feature, f64)> {
        let mut missing: Vec<(Feature, f64)> = Feature::ALL
            .into_iter()
            .filter(|f| *f != Feature::Address && !self.get(*f).is_present())
            .map(|f| (f, f.weight(weights)))
            .collect();
        missing.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.label().cmp(b.0.label()))
        });
        missing
    }

    /// Features whose value is a stand-in for the designed datum.
    pub fn derived(&self) -> Vec<Feature> {
        Feature::ALL
            .into_iter()
            .filter(|f| self.get(*f) == Datum::Derived)
            .collect()
    }

    /// How much of the designed model this feed can speak to.
    pub fn coverage(&self, weights: &ScoringWeights) -> Coverage {
        Coverage::new(
            self.available_weight(weights),
            self.designed_weight(weights),
        )
    }
}

/// The fraction of the designed scoring model a feed could exercise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coverage {
    /// Weight of the features that had a value.
    pub available_weight: f64,
    /// Weight of the whole designed model.
    pub designed_weight: f64,
}

impl Coverage {
    pub(crate) fn new(available_weight: f64, designed_weight: f64) -> Self {
        Self {
            available_weight,
            designed_weight,
        }
    }

    /// `available / designed`, `0.0` when the designed weight is zero (weights all
    /// turned off, which leaves nothing to measure).
    pub fn ratio(&self) -> f64 {
        if self.designed_weight <= 0.0 {
            0.0
        } else {
            self.available_weight / self.designed_weight
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn designed_weight_is_the_sum_of_the_scored_features() {
        let weights = ScoringWeights::default();
        let sources = FeatureSources::all_direct();
        // 40 + 30 + 25 + 15 + 15 + 20 + 25 + 10 + 5, with the address bypass excluded.
        assert_eq!(sources.designed_weight(&weights), 185.0);
    }

    #[test]
    fn a_full_feed_covers_the_whole_model() {
        let weights = ScoringWeights::default();
        assert_eq!(FeatureSources::all_direct().coverage(&weights).ratio(), 1.0);
    }

    #[test]
    fn an_undeclared_feed_covers_none_of_it() {
        let weights = ScoringWeights::default();
        let sources = FeatureSources::new();
        assert_eq!(sources.coverage(&weights).ratio(), 0.0);
    }

    #[test]
    fn a_missing_feature_leaves_the_denominator_rather_than_the_score() {
        let weights = ScoringWeights::default();
        let sources = FeatureSources::new()
            .with(Feature::ManufacturerId, Datum::Direct)
            .with(Feature::TimeContinuity, Datum::Direct);
        let coverage = sources.coverage(&weights);
        assert_eq!(coverage.available_weight, 65.0);
        assert!((coverage.ratio() - 65.0 / 185.0).abs() < 1e-12);
    }

    #[test]
    fn derived_data_still_counts_as_available() {
        let weights = ScoringWeights::default();
        let sources = FeatureSources::new().with(Feature::ServiceUuids, Datum::Derived);
        assert_eq!(sources.available_weight(&weights), 30.0);
        assert_eq!(sources.derived(), vec![Feature::ServiceUuids]);
    }

    #[test]
    fn missing_is_weighted_heaviest_first() {
        let weights = ScoringWeights::default();
        let missing = FeatureSources::new().missing(&weights);
        assert_eq!(missing.len(), 9);
        assert_eq!(missing[0].0, Feature::ManufacturerId);
        assert_eq!(missing.last().unwrap().0, Feature::Connectable);
    }

    #[test]
    fn the_address_bypass_carries_no_designed_weight() {
        let weights = ScoringWeights::default();
        let address_only = FeatureSources::new().with(Feature::Address, Datum::Direct);
        assert_eq!(address_only.available_weight(&weights), 0.0);
        assert_eq!(address_only.present(), vec![Feature::Address]);
    }

    #[test]
    fn zeroed_weights_cannot_be_a_denominator() {
        let weights = ScoringWeights {
            manufacturer_id: 0.0,
            uuid_overlap: 0.0,
            appearance: 0.0,
            field_layout: 0.0,
            payload_similarity: 0.0,
            time_continuity: 0.0,
            rssi: 0.0,
            name: 0.0,
            connectable: 0.0,
        };
        assert_eq!(FeatureSources::all_direct().coverage(&weights).ratio(), 0.0);
    }
}
