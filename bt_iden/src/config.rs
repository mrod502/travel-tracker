//! Configuration for the identity resolver.
//!
//! This module provides builder-style configuration for customizing the
//! behavior of the identity resolution engine.

use crate::models::ScoringWeights;
use std::time::Duration;

/// Configuration for the identity resolver.
///
/// # Example
///
/// ```
/// use bt_iden::{ResolverConfig, ScoringWeights};
/// use std::time::Duration;
///
/// let config = ResolverConfig::default()
///     .with_merge_threshold(100.0)
///     .with_matching_window(Duration::from_secs(120))
///     .with_weights(ScoringWeights {
///         manufacturer_id: 40.0,
///         ..ScoringWeights::default()
///     });
/// ```
#[derive(Debug, Clone)]
pub struct ResolverConfig {
    /// Score threshold for merging observations into an existing identity.
    pub merge_threshold: f64,
    /// Score threshold for considering a potential match (internal use).
    pub possible_threshold: f64,
    /// Time window for considering identities as active candidates.
    pub matching_window: Duration,
    /// Maximum age before an identity is considered expired.
    pub max_identity_age: Duration,
    /// Scoring weights for different matching features.
    pub weights: ScoringWeights,
    /// Minimum fraction of the designed model that must have had something to say
    /// before an inference-based merge is allowed.
    ///
    /// `merge_threshold` says how much evidence a merge needs; this says how much
    /// *evidence there could have been*. A feed that cannot see AD structures,
    /// appearance or connectability has a lower ceiling than the one the thresholds
    /// were written against, and without a floor here a merge decided on a third of
    /// the designed model would look exactly like one decided on all of it.
    ///
    /// An exact address match is exempt: it identifies the device rather than
    /// inferring it, so no amount of missing data makes it weaker.
    pub min_evidence_ratio: f64,
    /// Multiplier applied to points earned from a feature whose value stands in for
    /// the designed datum ([`Datum::Derived`](crate::evidence::Datum::Derived)).
    ///
    /// `1.0` treats a proxy as the real thing; `0.0` ignores it. Half keeps derived
    /// data useful — it corroborates other features — while making it unable to
    /// carry a merge on its own.
    pub derived_evidence_factor: f64,
    /// Maximum RSSI window size for rolling statistics.
    pub rssi_window_size: usize,
    /// Whether to enable debug logging for match decisions.
    pub debug_logging: bool,
}

impl Default for ResolverConfig {
    fn default() -> Self {
        Self {
            merge_threshold: 40.0,
            possible_threshold: 25.0,
            matching_window: Duration::from_secs(60),
            max_identity_age: Duration::from_secs(300),
            weights: ScoringWeights::default(),
            min_evidence_ratio: 0.35,
            derived_evidence_factor: 0.5,
            rssi_window_size: 10,
            debug_logging: false,
        }
    }
}

impl ResolverConfig {
    /// Creates a new configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the merge threshold score.
    ///
    /// Observations scoring at or above this value are merged with an existing
    /// identity, provided [`ResolverConfig::min_evidence_ratio`] is met as well.
    /// Default is 40.0.
    pub fn with_merge_threshold(mut self, threshold: f64) -> Self {
        self.merge_threshold = threshold;
        self
    }

    /// Sets the possible match threshold (internal use).
    ///
    /// Scores below this value are not candidates at all.
    /// Default is 25.0.
    pub fn with_possible_threshold(mut self, threshold: f64) -> Self {
        self.possible_threshold = threshold;
        self
    }

    /// Sets the matching window duration.
    ///
    /// Only identities observed within this window will be considered
    /// as match candidates. Default is 60 seconds.
    pub fn with_matching_window(mut self, duration: Duration) -> Self {
        self.matching_window = duration;
        self
    }

    /// Sets the maximum identity age.
    ///
    /// Identities not observed within this period will be expired.
    /// Default is 300 seconds (5 minutes).
    pub fn with_max_identity_age(mut self, duration: Duration) -> Self {
        self.max_identity_age = duration;
        self
    }

    /// Sets the scoring weights.
    pub fn with_weights(mut self, weights: ScoringWeights) -> Self {
        self.weights = weights;
        self
    }

    /// Sets the minimum coverage of the designed model an inference-based merge needs.
    ///
    /// See [`ResolverConfig::min_evidence_ratio`]. `0.0` restores the behaviour of
    /// judging on the absolute score alone.
    pub fn with_min_evidence_ratio(mut self, ratio: f64) -> Self {
        self.min_evidence_ratio = ratio;
        self
    }

    /// Sets how much a feature derived from a proxy datum is worth.
    ///
    /// See [`ResolverConfig::derived_evidence_factor`].
    pub fn with_derived_evidence_factor(mut self, factor: f64) -> Self {
        self.derived_evidence_factor = factor;
        self
    }

    /// Sets the RSSI window size for rolling statistics.
    pub fn with_rssi_window_size(mut self, size: usize) -> Self {
        self.rssi_window_size = size;
        self
    }

    /// Enables or disables debug logging.
    pub fn with_debug_logging(mut self, enabled: bool) -> Self {
        self.debug_logging = enabled;
        self
    }

    /// Returns the reject threshold (below possible threshold).
    pub fn reject_threshold(&self) -> f64 {
        self.possible_threshold
    }
}

/// Builder for creating resolver configuration.
///
/// # Example
///
/// ```
/// use bt_iden::config::ResolverConfig;
/// use std::time::Duration;
///
/// let config = ResolverConfig::builder()
///     .merge_threshold(100.0)
///     .matching_window(Duration::from_secs(120))
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct ResolverConfigBuilder {
    merge_threshold: Option<f64>,
    possible_threshold: Option<f64>,
    matching_window: Option<Duration>,
    max_identity_age: Option<Duration>,
    weights: Option<ScoringWeights>,
    min_evidence_ratio: Option<f64>,
    derived_evidence_factor: Option<f64>,
    rssi_window_size: Option<usize>,
    debug_logging: Option<bool>,
}

impl ResolverConfigBuilder {
    /// Creates a new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the merge threshold.
    pub fn merge_threshold(mut self, value: f64) -> Self {
        self.merge_threshold = Some(value);
        self
    }

    /// Sets the possible threshold.
    pub fn possible_threshold(mut self, value: f64) -> Self {
        self.possible_threshold = Some(value);
        self
    }

    /// Sets the matching window.
    pub fn matching_window(mut self, value: Duration) -> Self {
        self.matching_window = Some(value);
        self
    }

    /// Sets the maximum identity age.
    pub fn max_identity_age(mut self, value: Duration) -> Self {
        self.max_identity_age = Some(value);
        self
    }

    /// Sets the scoring weights.
    pub fn weights(mut self, value: ScoringWeights) -> Self {
        self.weights = Some(value);
        self
    }

    /// Sets the minimum coverage of the designed model an inference-based merge needs.
    pub fn min_evidence_ratio(mut self, value: f64) -> Self {
        self.min_evidence_ratio = Some(value);
        self
    }

    /// Sets how much a feature derived from a proxy datum is worth.
    pub fn derived_evidence_factor(mut self, value: f64) -> Self {
        self.derived_evidence_factor = Some(value);
        self
    }

    /// Sets the RSSI window size.
    pub fn rssi_window_size(mut self, value: usize) -> Self {
        self.rssi_window_size = Some(value);
        self
    }

    /// Enables or disables debug logging.
    pub fn debug_logging(mut self, value: bool) -> Self {
        self.debug_logging = Some(value);
        self
    }

    /// Builds the configuration.
    pub fn build(self) -> ResolverConfig {
        let mut config = ResolverConfig::default();

        if let Some(v) = self.merge_threshold {
            config.merge_threshold = v;
        }
        if let Some(v) = self.possible_threshold {
            config.possible_threshold = v;
        }
        if let Some(v) = self.matching_window {
            config.matching_window = v;
        }
        if let Some(v) = self.max_identity_age {
            config.max_identity_age = v;
        }
        if let Some(v) = self.weights {
            config.weights = v;
        }
        if let Some(v) = self.min_evidence_ratio {
            config.min_evidence_ratio = v;
        }
        if let Some(v) = self.derived_evidence_factor {
            config.derived_evidence_factor = v;
        }
        if let Some(v) = self.rssi_window_size {
            config.rssi_window_size = v;
        }
        if let Some(v) = self.debug_logging {
            config.debug_logging = v;
        }

        config
    }
}

impl ResolverConfig {
    /// Returns a builder for creating modified configuration.
    pub fn builder() -> ResolverConfigBuilder {
        ResolverConfigBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ResolverConfig::default();
        assert_eq!(config.merge_threshold, 40.0);
        assert_eq!(config.possible_threshold, 25.0);
        assert_eq!(config.matching_window, Duration::from_secs(60));
    }

    #[test]
    fn test_config_builder() {
        let config = ResolverConfig::builder()
            .merge_threshold(100.0)
            .matching_window(Duration::from_secs(120))
            .debug_logging(true)
            .build();

        assert_eq!(config.merge_threshold, 100.0);
        assert_eq!(config.matching_window, Duration::from_secs(120));
        assert!(config.debug_logging);
    }

    #[test]
    fn test_config_with_methods() {
        let config = ResolverConfig::new()
            .with_merge_threshold(90.0)
            .with_matching_window(Duration::from_secs(30));

        assert_eq!(config.merge_threshold, 90.0);
        assert_eq!(config.matching_window, Duration::from_secs(30));
    }
}
