//! Tests for the position vocabulary and the composition rules.

use super::*;
use crate::node::SystemClock;
use repo::models::LocationSource;
use std::sync::Arc;
use std::time::Duration;

use test_support::{gps_fix, gps_fix_aged, ManualClock};

fn fixed_source(lat: f64) -> Arc<dyn PositionSource> {
    Arc::new(MockPositionSource::from_positions(vec![gps_fix(lat)]))
}

fn position_config(raw: &str) -> PositionConfig {
    toml::from_str(raw).expect("test fixture should deserialize")
}

/// `unwrap_err` needs `T: Debug`, which a trait object is not.
fn expect_config_error(result: Result<Arc<dyn PositionSource>, PositionError>) -> PositionError {
    match result {
        Ok(_) => panic!("expected a configuration error, got a working source"),
        Err(error) => error,
    }
}

#[test]
fn coordinates_are_validated_on_construction() {
    assert!(Position::new(90.0, 180.0, PositionOrigin::Gps, test_support::epoch()).is_ok());
    assert!(matches!(
        Position::new(90.001, 0.0, PositionOrigin::Gps, test_support::epoch()),
        Err(PositionError::InvalidLatitude { .. })
    ));
    assert!(matches!(
        Position::new(0.0, -180.001, PositionOrigin::Gps, test_support::epoch()),
        Err(PositionError::InvalidLongitude { .. })
    ));
    // f64::parse happily accepts "NaN", so non-finite input has to be caught too.
    assert!(matches!(
        Position::new(f64::NAN, 0.0, PositionOrigin::Gps, test_support::epoch()),
        Err(PositionError::InvalidLatitude { .. })
    ));
    assert!(matches!(
        Position::new(0.0, f64::INFINITY, PositionOrigin::Gps, test_support::epoch()),
        Err(PositionError::InvalidLongitude { .. })
    ));
}

#[test]
fn a_fix_from_the_future_is_not_stale() {
    let epoch = test_support::epoch();
    let position = Position::new(
        1.0,
        1.0,
        PositionOrigin::Gps,
        epoch + chrono::Duration::hours(1),
    )
    .unwrap();

    assert_eq!(position.age(epoch), Duration::from_secs(0));
    assert!(!position.is_stale(Duration::from_secs(1), epoch));
}

#[test]
fn a_fix_older_than_max_age_is_stale() {
    let epoch = test_support::epoch();

    let fresh = gps_fix_aged(1.0, Duration::from_secs(59));
    assert!(!fresh.is_stale(Duration::from_secs(60), epoch));

    let stale = gps_fix_aged(1.0, Duration::from_secs(61));
    assert!(stale.is_stale(Duration::from_secs(60), epoch));
    // Time passing can turn a tolerable fix into an intolerable one.
    assert!(fresh.is_stale(Duration::from_secs(60), epoch + chrono::Duration::seconds(3)));
}

#[test]
fn origin_labels_and_database_enum_agree() {
    assert_eq!(PositionOrigin::Fixed.as_str(), "fixed");
    assert_eq!(PositionOrigin::Gps.as_str(), "gps");
    assert_eq!(PositionOrigin::Mock.as_str(), "mock");

    assert_eq!(
        PositionOrigin::Fixed.location_source(),
        LocationSource::NodeFixed
    );
    // A simulated fix is stored as a GPS fix, which is what it stands in for.
    assert_eq!(PositionOrigin::Gps.location_source(), LocationSource::NodeGps);
    assert_eq!(PositionOrigin::Mock.location_source(), LocationSource::NodeGps);
}

#[test]
fn coordinate_pairs_are_parsed_leniently_but_not_gullibly() {
    assert_eq!(
        parse_coordinates("40.6892,-74.0445").unwrap(),
        (40.6892, -74.0445)
    );
    assert_eq!(parse_coordinates(" 40.6892 , -74.0445 ").unwrap(), (40.6892, -74.0445));
    assert_eq!(parse_coordinates("0,0").unwrap(), (0.0, 0.0));

    for raw in [
        "", "40.6892", "40.6892;", "lat,lon", "40.6892,-74.0445,10", ",,", "91,0", "0,181",
        "nan,0",
    ] {
        assert!(
            parse_coordinates(raw).is_err(),
            "should have been rejected: {raw:?}"
        );
    }
}

#[test]
fn config_defaults_never_probe_a_receiver() {
    let config = PositionConfig::default();

    assert_eq!(config.mode, PositionMode::Auto);
    assert_eq!(config.fixed, None);
    // The reason auto mode does not open a socket by itself.
    assert_eq!(config.gps, None);
    assert_eq!(config.fixed_coordinates().unwrap(), None);
}

#[test]
fn config_reads_a_receiver_section() {
    let config = position_config(
        r#"
mode = "auto"
fixed = "40.6892,-74.0445"

[gps]
backend = "gpsd"
host = "192.168.1.20"
port = 2947
timeout_ms = 3000
"#,
    );

    let gps = config.gps.clone().expect("section was present");
    assert_eq!(gps.backend, GpsBackend::Gpsd);
    assert_eq!(gps.host, "192.168.1.20");
    assert_eq!(gps.port, 2947);
    assert_eq!(gps.timeout_ms, 3000);
    // Unset keys keep their defaults.
    assert_eq!(gps.max_age_ms, 60_000);
    assert_eq!(gps.retry_backoff_ms, 30_000);
}

#[test]
fn config_rejects_unknown_keys_rather_than_ignoring_them() {
    assert!(toml::from_str::<PositionConfig>(r#"mode = "fixed""#).is_ok());
    assert!(toml::from_str::<PositionConfig>(r#"mode = "sideways""#).is_err());
    assert!(toml::from_str::<PositionConfig>(r#"fixed_position = "1,2""#).is_err());
    assert!(toml::from_str::<PositionConfig>(
        r#"
[gps]
baeckend = "gpsd"
"#
    )
    .is_err());
}

#[tokio::test]
async fn auto_mode_prefers_the_fixed_location() {
    let source =
        compose_sources(Some(fixed_source(1.0)), Some(fixed_source(2.0)), PositionMode::Auto)
            .unwrap();

    assert_eq!(
        source.current_position().await.unwrap().unwrap().latitude,
        1.0,
        "a point the operator chose outranks a receiver"
    );
}

#[tokio::test]
async fn auto_mode_uses_gps_when_nothing_is_fixed() {
    let source = compose_sources(None, Some(fixed_source(3.0)), PositionMode::Auto).unwrap();

    assert_eq!(
        source.current_position().await.unwrap().unwrap().latitude,
        3.0
    );
}

#[tokio::test]
async fn auto_mode_without_anything_configured_reports_no_position() {
    let source = compose_sources(None, None, PositionMode::Auto).unwrap();

    assert!(source.current_position().await.unwrap().is_none());
}

#[tokio::test]
async fn off_mode_ignores_what_is_configured() {
    let source = compose_sources(Some(fixed_source(4.0)), Some(fixed_source(5.0)), PositionMode::Off)
        .unwrap();

    assert!(source.current_position().await.unwrap().is_none());
}

#[test]
fn demanding_modes_fail_loudly_when_their_source_is_missing() {
    let fixed_only = expect_config_error(compose_sources(
        None,
        Some(fixed_source(1.0)),
        PositionMode::Fixed,
    ));
    assert!(matches!(fixed_only, PositionError::NotConfigured(_)));

    let gps_only = expect_config_error(compose_sources(
        Some(fixed_source(1.0)),
        None,
        PositionMode::Gps,
    ));
    assert!(matches!(gps_only, PositionError::NotConfigured(_)));
}

#[tokio::test]
async fn a_configured_fixed_point_is_reported_end_to_end() {
    let config = position_config(r#"mode = "fixed""#);
    let config = PositionConfig {
        fixed: Some("40.6892,-74.0445".to_string()),
        ..config
    };

    let source = config.build_source(Arc::new(SystemClock)).unwrap();
    let position = source.current_position().await.unwrap().unwrap();

    assert_eq!(position.origin, PositionOrigin::Fixed);
    assert_eq!(position.latitude, 40.6892);
}

#[tokio::test]
async fn a_simulated_receiver_produces_location_for_a_node_without_hardware() {
    let config = position_config(
        r#"
mode = "gps"
fixed = "40.6892,-74.0445"

[gps]
backend = "mock"
"#,
    );
    let clock = Arc::new(ManualClock::new(test_support::epoch()));

    let source = config.build_source(clock.clone()).unwrap();
    let position = source.current_position().await.unwrap().unwrap();

    assert_eq!(position.origin, PositionOrigin::Mock);
    assert_eq!(position.latitude, 40.6892);
    assert_eq!(position.fixed_at, test_support::epoch());
}

#[test]
fn a_simulated_receiver_needs_a_centre_to_drift_from() {
    let config = position_config(
        r#"
mode = "gps"

[gps]
backend = "mock"
"#,
    );

    let error = expect_config_error(config.build_source(Arc::new(SystemClock)));
    assert!(matches!(error, PositionError::NotConfigured(_)));
}

#[test]
fn a_malformed_fixed_point_is_a_startup_error() {
    let config = PositionConfig {
        mode: PositionMode::Fixed,
        fixed: Some("somewhere".to_string()),
        gps: None,
    };

    let error = expect_config_error(config.build_source(Arc::new(SystemClock)));
    assert!(matches!(
        error,
        PositionError::InvalidCoordinateString { .. }
    ));
}
