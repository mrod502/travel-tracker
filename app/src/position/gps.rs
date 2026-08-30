//! gpsd JSON receiver.
//!
//! Each acquisition opens a connection, subscribes with a `WATCH` request and
//! waits for the first position report. That is a round trip per refresh rather
//! than a standing subscription, which is deliberate: [`CachedPositionSource`]
//! absorbs the repeat calls, and a node that loses its receiver should recover by
//! reconnecting rather than by resynchronising a half-read socket.

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

use super::{Position, PositionError, PositionOrigin, PositionSource};

/// Ask gpsd to stream JSON objects including TPV reports.
const WATCH_REQUEST: &[u8] =
    b"{\"class\":\"WATCH\",\"enable\":true,\"json\":true,\"enableTPV\":true}\n";

/// A position report from a receiver.
#[derive(Debug, Clone, PartialEq)]
pub struct GpsFix {
    /// Reported latitude in degrees.
    pub latitude: f64,
    /// Reported longitude in degrees.
    pub longitude: f64,
    /// Altitude above mean sea level in metres.
    pub altitude_m: Option<f64>,
    /// Horizontal accuracy in metres, derived from the reported error estimates.
    pub accuracy_m: Option<f64>,
    /// When the receiver says it acquired the fix, which is not when we read it.
    pub acquired_at: Option<DateTime<Utc>>,
}

impl GpsFix {
    /// Turn the report into a [`Position`].
    ///
    /// A receiver that reports no timestamp is stamped `now`: the alternative is
    /// to discard an otherwise good fix because a particular gpsd build omitted
    /// an optional field.
    pub fn to_position(&self, now: DateTime<Utc>) -> Result<Position, PositionError> {
        Position::with_detail(
            self.latitude,
            self.longitude,
            self.altitude_m,
            self.accuracy_m,
            PositionOrigin::Gps,
            self.acquired_at.unwrap_or(now),
        )
    }
}

/// How a [`GpsPositionSource`] obtains reports.
///
/// Separated from the source so the parsing and mapping logic can be tested
/// without a receiver, and so a transport other than gpsd can be plugged in.
#[async_trait]
pub trait GpsTransport: Send + Sync {
    /// Wait up to `wait` for the next position report.
    async fn next_fix(&self, wait: Duration) -> Result<GpsFix, PositionError>;
}

/// gpsd over TCP.
pub struct GpsdTransport {
    host: String,
    port: u16,
}

impl GpsdTransport {
    /// Transport talking to gpsd at `host:port`.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

#[async_trait]
impl GpsTransport for GpsdTransport {
    async fn next_fix(&self, wait: Duration) -> Result<GpsFix, PositionError> {
        timeout(wait, self.read_first_fix())
            .await
            .map_err(|_| PositionError::Timeout { timeout: wait })?
    }
}

impl GpsdTransport {
    async fn read_first_fix(&self) -> Result<GpsFix, PositionError> {
        let stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|error| {
                PositionError::Transport(format!(
                    "cannot reach gpsd at {}:{}: {}",
                    self.host, self.port, error
                ))
            })?;

        let (read_half, mut write_half) = stream.into_split();
        write_half
            .write_all(WATCH_REQUEST)
            .await
            .map_err(|error| {
                PositionError::Transport(format!("could not subscribe to gpsd reports: {}", error))
            })?;

        let mut lines = BufReader::new(read_half).lines();
        while let Some(line) = lines.next_line().await.map_err(|error| {
            PositionError::Transport(format!("gpsd stream failed: {}", error))
        })? {
            match parse_report(&line) {
                Ok(Some(fix)) => return Ok(fix),
                Ok(None) => continue,
                Err(error) => return Err(error),
            }
        }

        Err(PositionError::Protocol(
            "gpsd closed the connection before reporting a fix".to_string(),
        ))
    }
}

/// Reads the next report from `host:port` and caches it.
pub struct GpsPositionSource<T: GpsTransport = GpsdTransport> {
    transport: T,
    wait: Duration,
}

impl GpsPositionSource<GpsdTransport> {
    /// Source reading from gpsd, bounded by `wait` per acquisition.
    pub fn gpsd(host: impl Into<String>, port: u16, wait: Duration) -> Self {
        Self {
            transport: GpsdTransport::new(host, port),
            wait,
        }
    }
}

impl<T: GpsTransport> GpsPositionSource<T> {
    /// Source over an arbitrary transport.
    pub fn with_transport(transport: T, wait: Duration) -> Self {
        Self { transport, wait }
    }
}

#[async_trait]
impl<T: GpsTransport> PositionSource for GpsPositionSource<T> {
    async fn current_position(&self) -> Result<Option<Position>, PositionError> {
        let fix = self.transport.next_fix(self.wait).await?;
        Ok(Some(fix.to_position(Utc::now())?))
    }
}

/// One line of gpsd JSON output.
///
/// Every field is optional because gpsd emits several message classes on the same
/// socket and only the TPV ones matter here.
#[derive(Debug, Deserialize)]
struct GpsdMessage {
    #[serde(rename = "class", default)]
    class: Option<String>,
    #[serde(default)]
    mode: Option<u8>,
    #[serde(default)]
    lat: Option<f64>,
    #[serde(default)]
    lon: Option<f64>,
    #[serde(default)]
    alt: Option<f64>,
    #[serde(default)]
    epx: Option<f64>,
    #[serde(default)]
    epy: Option<f64>,
    /// gpsd labels this field `time` in TPV reports; `timestamp` shows up in
    /// other message classes and in older releases.
    #[serde(rename = "time", alias = "timestamp", default)]
    timestamp: Option<TimestampField>,
    #[serde(default)]
    message: Option<String>,
}

/// gpsd reports time either as fractional epoch seconds or as an ISO 8601 string,
/// depending on version and mode.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TimestampField {
    Seconds(f64),
    Text(String),
}

impl TimestampField {
    fn to_datetime(&self) -> Option<DateTime<Utc>> {
        match self {
            TimestampField::Seconds(secs) if secs.is_finite() => {
                let whole = secs.floor() as i64;
                let nanos = ((secs - secs.floor()) * 1e9).round() as u32;
                Utc.timestamp_opt(whole, nanos).single()
            }
            TimestampField::Text(text) => DateTime::parse_from_rfc3339(text)
                .ok()
                .map(|parsed| parsed.with_timezone(&Utc)),
            _ => None,
        }
    }
}

/// Interpret one line of gpsd output.
///
/// `Ok(None)` means "keep reading": a version banner, a SKY report, a line
/// without coordinates, or a receiver that has not got a fix yet. Lines that are
/// not JSON at all are skipped too, so a chatty serial source cannot wedge the
/// node.
pub fn parse_report(line: &str) -> Result<Option<GpsFix>, PositionError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }

    let Ok(message) = serde_json::from_str::<GpsdMessage>(line) else {
        return Ok(None);
    };

    match message.class.as_deref() {
        Some("ERROR") => Err(PositionError::Protocol(
            message.message.unwrap_or_else(|| "gpsd reported an error".to_string()),
        )),
        Some("TPV") => Ok(tpv_to_fix(&message)),
        _ => Ok(None),
    }
}

fn tpv_to_fix(message: &GpsdMessage) -> Option<GpsFix> {
    // mode 0 and 1 mean "no fix yet"; 2 is 2D and 3 is 3D. Older reports omit
    // mode entirely, in which case coordinates present are the fix signal.
    if message.mode.is_some_and(|mode| mode < 2) {
        return None;
    }
    let (Some(latitude), Some(longitude)) = (message.lat, message.lon) else {
        return None;
    };

    Some(GpsFix {
        latitude,
        longitude,
        altitude_m: message.alt.filter(|alt| alt.is_finite()),
        accuracy_m: horizontal_accuracy(message.epx, message.epy),
        acquired_at: message.timestamp.as_ref().and_then(|ts| ts.to_datetime()),
    })
}

/// Combine gpsd's separate east/north error estimates into one radius.
fn horizontal_accuracy(epx: Option<f64>, epy: Option<f64>) -> Option<f64> {
    let usable = |value: &f64| value.is_finite() && *value >= 0.0;

    match (epy.filter(usable), epx.filter(usable)) {
        (Some(north), Some(east)) => Some((north * north + east * east).sqrt()),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::test_support::epoch;

    #[test]
    fn parses_a_three_dimensional_report() {
        let line = r#"{"class":"TPV","device":"/dev/ttyACM0","mode":3,"time":"2026-08-29T10:11:12.000Z","lat":40.689246,"lon":-74.044502,"alt":4.2,"epx":3.1,"epy":2.9,"epv":5.0,"track":210.5,"speed":0.4,"climb":0.0}"#;

        let fix = parse_report(line)
            .unwrap()
            .expect("a mode 3 report is a fix");

        assert_eq!(fix.latitude, 40.689246);
        assert_eq!(fix.longitude, -74.044502);
        assert_eq!(fix.altitude_m, Some(4.2));
        assert_eq!(
            fix.acquired_at,
            Some(DateTime::parse_from_rfc3339("2026-08-29T10:11:12.000Z").unwrap().with_timezone(&Utc))
        );
    }

    #[test]
    fn accuracy_is_the_reported_error_radius() {
        let line = r#"{"class":"TPV","mode":2,"lat":1.0,"lon":2.0,"epx":3.0,"epy":4.0}"#;
        let fix = parse_report(line).unwrap().unwrap();
        assert!((fix.accuracy_m.unwrap() - 5.0).abs() < 1e-9);

        let line = r#"{"class":"TPV","mode":2,"lat":1.0,"lon":2.0,"epy":4.0}"#;
        assert_eq!(parse_report(line).unwrap().unwrap().accuracy_m, Some(4.0));

        let line = r#"{"class":"TPV","mode":2,"lat":1.0,"lon":2.0}"#;
        assert_eq!(parse_report(line).unwrap().unwrap().accuracy_m, None);
    }

    #[test]
    fn epoch_seconds_timestamps_are_accepted() {
        let line = r#"{"class":"TPV","mode":2,"lat":1.0,"lon":2.0,"timestamp":1700000000.5}"#;
        let fix = parse_report(line).unwrap().unwrap();

        assert_eq!(fix.acquired_at, Some(epoch() + chrono::Duration::milliseconds(500)));
    }

    #[test]
    fn reports_without_a_fix_are_ignored() {
        for line in [
            r#"{"class":"TPV","tag":"DEV","device":"/dev/ttyACM0","mode":0}"#,
            r#"{"class":"TPV","mode":1,"lat":0.0,"lon":0.0}"#,
            r#"{"class":"SKY","mode":1,"satellites":[]}"#,
            r#"{"class":"VERSION","release":"3.25"}"#,
            "not json at all",
            "",
            r#"{"class":"TPV","mode":3}"#,
        ] {
            assert_eq!(
                parse_report(line).unwrap(),
                None,
                "should keep reading instead of reporting: {line}"
            );
        }
    }

    #[test]
    fn an_error_report_is_a_protocol_error() {
        let line = r#"{"class":"ERROR","message":"No DOF for these devices"}"#;

        let error = parse_report(line).unwrap_err();
        assert!(matches!(&error, PositionError::Protocol(text) if text == "No DOF for these devices"));
    }

    #[test]
    fn out_of_range_coordinates_are_rejected_when_mapped() {
        let fix = GpsFix {
            latitude: 91.0,
            longitude: 0.0,
            altitude_m: None,
            accuracy_m: None,
            acquired_at: None,
        };

        assert!(matches!(
            fix.to_position(Utc::now()),
            Err(PositionError::InvalidLatitude { .. })
        ));
    }

    #[tokio::test]
    async fn a_report_without_a_timestamp_is_stamped_when_read() {
        let source = GpsPositionSource::with_transport(
            ScriptedTransport(vec![GpsFix {
                latitude: 5.0,
                longitude: 6.0,
                altitude_m: None,
                accuracy_m: None,
                acquired_at: None,
            }]),
            Duration::from_millis(50),
        );

        let position = source.current_position().await.unwrap().unwrap();
        assert_eq!(position.origin, PositionOrigin::Gps);
        assert!(position.fixed_at <= Utc::now());
        assert!(position.age(Utc::now()) < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn transport_errors_pass_through() {
        let source = GpsPositionSource::with_transport(
            ScriptedTransport(vec![]),
            Duration::from_millis(50),
        );

        assert!(matches!(
            source.current_position().await.unwrap_err(),
            PositionError::Protocol(_)
        ));
    }

    /// Test transport handing out canned reports, then nothing.
    struct ScriptedTransport(Vec<GpsFix>);

    #[async_trait]
    impl GpsTransport for ScriptedTransport {
        async fn next_fix(&self, _wait: Duration) -> Result<GpsFix, PositionError> {
            self.0
                .first()
                .cloned()
                .ok_or_else(|| PositionError::Protocol("no canned reports left".to_string()))
        }
    }
}
