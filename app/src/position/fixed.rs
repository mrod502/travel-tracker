//! Operator-configured fixed coordinates.

use async_trait::async_trait;
use chrono::Utc;

use super::{Position, PositionError, PositionOrigin, PositionSource};

/// Reports the same coordinates on every call.
///
/// Coordinates are range-checked when the configuration is parsed, and checked
/// again on every report so a hand-constructed source cannot emit a position the
/// database would reject.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixedPositionSource {
    latitude: f64,
    longitude: f64,
}

impl FixedPositionSource {
    /// Source that always reports `(latitude, longitude)`.
    pub fn new(latitude: f64, longitude: f64) -> Self {
        Self {
            latitude,
            longitude,
        }
    }

    /// The coordinates this source reports.
    pub fn coordinates(&self) -> (f64, f64) {
        (self.latitude, self.longitude)
    }
}

#[async_trait]
impl PositionSource for FixedPositionSource {
    async fn current_position(&self) -> Result<Option<Position>, PositionError> {
        // A configured point is always current; there is no acquisition step
        // that could have failed or taken time.
        let position = Position::new(
            self.latitude,
            self.longitude,
            PositionOrigin::Fixed,
            Utc::now(),
        )?;
        Ok(Some(position))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use repo::models::LocationSource;

    #[tokio::test]
    async fn reports_configured_coordinates() {
        let source = FixedPositionSource::new(40.6892, -74.0445);

        let position = source
            .current_position()
            .await
            .unwrap()
            .expect("fixed source always reports");

        assert_eq!(position.latitude, 40.6892);
        assert_eq!(position.longitude, -74.0445);
        assert_eq!(position.origin, PositionOrigin::Fixed);
        assert_eq!(position.location_source(), LocationSource::NodeFixed);
    }

    #[tokio::test]
    async fn rejects_out_of_range_coordinates_at_report_time() {
        let source = FixedPositionSource::new(91.0, 0.0);

        let error = source.current_position().await.unwrap_err();
        assert!(matches!(
            error,
            PositionError::InvalidLatitude { value: 91.0 }
        ));
    }
}
