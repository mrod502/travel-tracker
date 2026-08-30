//! Position acquisition for location-stamped occurrences.
//!
//! A node's position is asserted inside the signed canonical payload (field 9
//! of the Phase 0 spec), so a location is a claim about the node at the moment
//! of observation rather than a hint. That constraint shapes everything here:
//!
//! * Every [`Position`] carries the instant it was acquired (`fixed_at`) and the
//!   [`PositionOrigin`] it came from, so a stale or simulated fix is always
//!   identifiable after the fact.
//! * [`CachedPositionSource`] never serves a stale fix when the underlying source
//!   fails — a wrong position signed into a payload is worse than a NULL location.
//! * Sources are [`PositionSource`] trait objects so the acquisition mechanism
//!   (fixed config, gpsd, a simulator, or something added later) is a wiring
//!   decision, not a code change in the node.
//!
//! # Composition
//!
//! [`build_position_source`] assembles the chain described by a
//! [`PositionConfig`]:
//!
//! ```text
//! mode = auto      fixed (if configured)  →  cached gps (if configured)
//! mode = fixed     fixed only
//! mode = gps       cached gps only
//! mode = off       NoPositionSource
//! ```
//!
//! # Example
//!
//! ```ignore
//! use app::position::{PositionConfig, build_position_source};
//! use app::node::SystemClock;
//! use std::sync::Arc;
//!
//! let config: PositionConfig = toml::from_str(r#"
//!     mode = "auto"
//!     fixed = "40.6892,-74.0445"
//! "#).unwrap();
//!
//! let source = build_position_source(&config, Arc::new(SystemClock))?;
//! if let Some(position) = source.current_position().await? {
//!     println!("{}, {} from {:?}", position.latitude, position.longitude, position.origin);
//! }
//! ```

pub mod cache;
pub mod fallback;
pub mod fixed;
pub mod gps;
pub mod mock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use repo::models::LocationSource;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub use cache::CachedPositionSource;
pub use fallback::FallbackPositionSource;
pub use fixed::FixedPositionSource;
pub use mock::MockPositionSource;

use crate::node::Clock;

/// Where an acquired position came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionOrigin {
    /// Operator-supplied fixed coordinates.
    Fixed,
    /// A real receiver (gpsd).
    Gps,
    /// A simulated fix, used when no hardware is available.
    Mock,
}

impl PositionOrigin {
    /// Stable identifier for `signal_payload` and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            PositionOrigin::Fixed => "fixed",
            PositionOrigin::Gps => "gps",
            PositionOrigin::Mock => "mock",
        }
    }

    /// The `location_source` enum value for the occurrences table.
    ///
    /// A simulated fix is recorded as `node_gps` because that is what it stands
    /// in for; the `mock` label survives in `signal_payload`, so test data stays
    /// distinguishable without inventing a fifth enum variant.
    pub fn location_source(self) -> LocationSource {
        match self {
            PositionOrigin::Fixed => LocationSource::NodeFixed,
            PositionOrigin::Gps | PositionOrigin::Mock => LocationSource::NodeGps,
        }
    }
}

/// Errors from position acquisition.
#[derive(Error, Debug, Clone)]
pub enum PositionError {
    /// A source was requested but nothing was configured to satisfy it.
    #[error("no position source configured: {0}")]
    NotConfigured(String),

    #[error("invalid latitude {value}: expected a finite value in [-90, 90]")]
    InvalidLatitude { value: f64 },

    #[error("invalid longitude {value}: expected a finite value in [-180, 180]")]
    InvalidLongitude { value: f64 },

    /// The transport could not be used at all (socket, parse of the request).
    #[error("position transport failed: {0}")]
    Transport(String),

    /// The transport answered, but said something unusable.
    #[error("position source protocol error: {0}")]
    Protocol(String),

    /// No report arrived within the configured wait.
    #[error("no position reported within {timeout:?}")]
    Timeout { timeout: Duration },

    /// A report arrived, but it was older than the node is willing to assert.
    #[error("position fix is {age:?} old, beyond the configured maximum")]
    StaleFix { age: Duration },

    /// Malformed coordinate pair in configuration.
    #[error("invalid coordinates '{raw}': expected 'lat,lon' (e.g. '40.6892,-74.0445')")]
    InvalidCoordinateString { raw: String },
}

/// A single position observation.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    /// WGS84 latitude in degrees.
    pub latitude: f64,
    /// WGS84 longitude in degrees.
    pub longitude: f64,
    /// Altitude above mean sea level in metres, when reported.
    pub altitude_m: Option<f64>,
    /// Horizontal accuracy estimate in metres, when reported.
    pub accuracy_m: Option<f64>,
    /// When this position was acquired, not when it was consumed.
    pub fixed_at: DateTime<Utc>,
    /// Which kind of source produced it.
    pub origin: PositionOrigin,
}

impl Position {
    /// Build a position with only the mandatory fields.
    pub fn new(
        latitude: f64,
        longitude: f64,
        origin: PositionOrigin,
        fixed_at: DateTime<Utc>,
    ) -> Result<Self, PositionError> {
        Self::with_detail(latitude, longitude, None, None, origin, fixed_at)
    }

    /// Build a position, validating the coordinates are finite and in range.
    pub fn with_detail(
        latitude: f64,
        longitude: f64,
        altitude_m: Option<f64>,
        accuracy_m: Option<f64>,
        origin: PositionOrigin,
        fixed_at: DateTime<Utc>,
    ) -> Result<Self, PositionError> {
        if !latitude.is_finite() || latitude < -90.0 || latitude > 90.0 {
            return Err(PositionError::InvalidLatitude { value: latitude });
        }
        if !longitude.is_finite() || longitude < -180.0 || longitude > 180.0 {
            return Err(PositionError::InvalidLongitude { value: longitude });
        }

        Ok(Self {
            latitude,
            longitude,
            altitude_m,
            accuracy_m,
            fixed_at,
            origin,
        })
    }

    /// How old this fix is at `now`. Zero when `now` precedes the fix.
    pub fn age(&self, now: DateTime<Utc>) -> Duration {
        (now - self.fixed_at)
            .to_std()
            .unwrap_or(Duration::from_secs(0))
    }

    /// Whether the fix is older than `max_age` at `now`.
    pub fn is_stale(&self, max_age: Duration, now: DateTime<Utc>) -> bool {
        self.age(now) > max_age
    }

    /// The `location_source` value to store alongside this position.
    pub fn location_source(&self) -> LocationSource {
        self.origin.location_source()
    }
}

/// Something that can report where the node currently is.
///
/// Implementations must be cheap enough to call once per rate-limited
/// occurrence; [`CachedPositionSource`] exists so the node does not drive a
/// receiver that often.
#[async_trait]
pub trait PositionSource: Send + Sync {
    /// `Ok(Some(_))` for a usable fix, `Ok(None)` when the source is healthy but
    /// has nothing to report yet, and `Err` when the source could not be reached
    /// or reported something invalid.
    async fn current_position(&self) -> Result<Option<Position>, PositionError>;
}

/// Explicit "this node has no location" source.
///
/// Distinct from a source that fails: it is the configured state, so callers
/// must not warn about it.
pub struct NoPositionSource;

#[async_trait]
impl PositionSource for NoPositionSource {
    async fn current_position(&self) -> Result<Option<Position>, PositionError> {
        Ok(None)
    }
}

/// How the node should acquire its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionMode {
    /// Fixed location if configured, otherwise GPS.
    #[default]
    Auto,
    /// Only the configured fixed location.
    Fixed,
    /// Only a receiver.
    Gps,
    /// Never report a position.
    Off,
}

/// Which receiver to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpsBackend {
    /// gpsd's JSON protocol over TCP.
    #[default]
    Gpsd,
    /// Simulated track, for development without a receiver.
    Mock,
}

/// Receiver settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GpsSettings {
    /// Which receiver to talk to.
    pub backend: GpsBackend,
    /// gpsd host.
    pub host: String,
    /// gpsd port.
    pub port: u16,
    /// How long to wait for a report before giving up.
    pub timeout_ms: u64,
    /// How long an acquired fix may be reused.
    pub max_age_ms: u64,
    /// How long to stop retrying after a failure, so a node with no receiver
    /// does not attempt a connection per occurrence.
    pub retry_backoff_ms: u64,
}

impl Default for GpsSettings {
    fn default() -> Self {
        Self {
            backend: GpsBackend::default(),
            host: "127.0.0.1".to_string(),
            port: 2947,
            timeout_ms: 2_000,
            max_age_ms: 60_000,
            retry_backoff_ms: 30_000,
        }
    }
}

/// `[location]` settings, resolved from flags, config file and env.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PositionConfig {
    /// Acquisition strategy.
    pub mode: PositionMode,
    /// Fixed coordinates as `"lat,lon"`.
    pub fixed: Option<String>,
    /// Receiver settings. `None` means no receiver is configured, so `auto`
    /// falls back to no position rather than probing a port nobody listens on.
    pub gps: Option<GpsSettings>,
}

impl PositionConfig {
    /// The parsed fixed coordinates, when configured.
    pub fn fixed_coordinates(&self) -> Result<Option<(f64, f64)>, PositionError> {
        self.fixed.as_deref().map(parse_coordinates).transpose()
    }

    /// Build the configured source chain.
    ///
    /// `clock` timestamps simulated fixes and ages cached ones, so tests can
    /// drive it without waiting.
    pub fn build_source(
        &self,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<dyn PositionSource>, PositionError> {
        build_position_source(self, clock)
    }
}

/// Parse a `"lat,lon"` coordinate pair.
pub fn parse_coordinates(raw: &str) -> Result<(f64, f64), PositionError> {
    let invalid = || PositionError::InvalidCoordinateString { raw: raw.to_string() };

    let mut parts = raw.split(',');
    let latitude = parts.next().and_then(|p| p.trim().parse::<f64>().ok());
    let longitude = parts.next().and_then(|p| p.trim().parse::<f64>().ok());
    let (Some(latitude), Some(longitude)) = (latitude, longitude) else {
        return Err(invalid());
    };
    if parts.next().is_some() {
        return Err(invalid());
    }

    // Range-check now rather than at every use site: a typo in a config file is
    // a startup error, not a per-occurrence one.
    Position::new(latitude, longitude, PositionOrigin::Fixed, Utc::now())
        .map(|p| (p.latitude, p.longitude))
        .map_err(|_| invalid())
}

/// Assemble the source chain described by `config`.
pub fn build_position_source(
    config: &PositionConfig,
    clock: Arc<dyn Clock>,
) -> Result<Arc<dyn PositionSource>, PositionError> {
    let coordinates = config.fixed_coordinates()?;
    let fixed = coordinates
        .map(|(lat, lon)| Arc::new(FixedPositionSource::new(lat, lon)) as Arc<dyn PositionSource>);

    let gps = config
        .gps
        .as_ref()
        .map(|settings| build_gps_source(settings, coordinates, clock.clone()))
        .transpose()?
        .map(|source| Arc::new(source) as Arc<dyn PositionSource>);

    compose_sources(fixed, gps, config.mode)
}

/// Apply [`PositionMode`] to the sources that happen to be configured.
///
/// Split out from [`build_position_source`] so the selection rules can be tested
/// without constructing any particular transport.
pub(crate) fn compose_sources(
    fixed: Option<Arc<dyn PositionSource>>,
    gps: Option<Arc<dyn PositionSource>>,
    mode: PositionMode,
) -> Result<Arc<dyn PositionSource>, PositionError> {
    let sources: Vec<Arc<dyn PositionSource>> = match mode {
        PositionMode::Off => Vec::new(),
        PositionMode::Fixed => vec![fixed.ok_or_else(|| {
            PositionError::NotConfigured(
                "mode is 'fixed' but no fixed location is configured".to_string(),
            )
        })?],
        PositionMode::Gps => vec![gps.ok_or_else(|| {
            PositionError::NotConfigured(
                "mode is 'gps' but no [location.gps] receiver is configured".to_string(),
            )
        })?],
        PositionMode::Auto => fixed.into_iter().chain(gps).collect(),
    };

    Ok(FallbackPositionSource::compose(sources))
}

/// Wrap a receiver in its cache, per [`GpsSettings`].
///
/// `fixed` is the configured fixed point, reused as the centre of the simulated
/// track when the mock backend is selected.
fn build_gps_source(
    settings: &GpsSettings,
    fixed: Option<(f64, f64)>,
    clock: Arc<dyn Clock>,
) -> Result<CachedPositionSource, PositionError> {
    let wait = Duration::from_millis(settings.timeout_ms);
    let source: Box<dyn PositionSource> = match settings.backend {
        GpsBackend::Gpsd => Box::new(gps::GpsPositionSource::gpsd(
            &settings.host,
            settings.port,
            wait,
        )),
        GpsBackend::Mock => {
            let centre = fixed.ok_or_else(|| {
                PositionError::NotConfigured(
                    "gps backend 'mock' needs location.fixed as the centre of the simulated track"
                        .to_string(),
                )
            })?;
            Box::new(MockPositionSource::drift(centre, clock.clone()))
        }
    };

    Ok(CachedPositionSource::new(
        Arc::from(source),
        Duration::from_millis(settings.max_age_ms),
        Duration::from_millis(settings.retry_backoff_ms),
        clock,
    ))
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
