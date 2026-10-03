//! The TOML configuration file shape.
//!
//! Every field is optional, and the optionality is not a convenience: the
//! resolver distinguishes "the file says 7000" from "the file says nothing" in
//! order to apply the precedence rule, so a section must not hold defaults.
//! Those live in [`super`].
//!
//! Section names end in `Section` to keep them distinct from the resolved
//! `*Config` structs the application reads.
//!
//! The newer sections (`[location]`, `[location.gps]`) reject unknown keys, so a
//! typo like `fixd = "…"` is reported rather than ignored. The older sections
//! stay permissive on purpose: a `config.toml` that works today must keep working
//! after an upgrade adds a field.

use serde::{Deserialize, Serialize};

use crate::position::{GpsBackend, PositionMode};

/// A parsed config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    pub database: Option<DatabaseSection>,
    pub log: Option<LogSection>,
    pub node: Option<NodeSection>,
    pub bluetooth: Option<BluetoothSection>,
    /// Position acquisition settings.
    #[serde(default)]
    pub location: Option<LocationSection>,
    /// Certificate authority settings, used by the `ca` subcommands.
    pub ca: Option<CaSection>,
}

/// `[database]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseSection {
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
}

/// `[log]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogSection {
    pub level: Option<String>,
}

/// `[node]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeSection {
    pub id: Option<String>,
    /// Res 6 H3 cells this node is authoritative for, as hex or decimal strings.
    ///
    /// A claim, not a fact derived from position: a node may own cells it is not
    /// inside, and a mobile node owns none.
    pub owns_cells: Option<Vec<String>>,
}

/// `[bluetooth]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BluetoothSection {
    pub adapter_id: Option<String>,
    pub scan_interval_ms: Option<u64>,
    pub store_raw_payload: Option<bool>,
    pub data_dir: Option<String>,
    pub rate_limit_ms: Option<u64>,
    /// Delay (ms) before reopening the device event stream after it closes.
    pub stream_reopen_delay_ms: Option<u64>,
    /// Use the mock Bluetooth backend instead of a physical adapter.
    ///
    /// Only takes effect when the application is built with the `mock` feature.
    pub use_mock_backend: Option<bool>,
    /// Deprecated: superseded by `[location].fixed`.
    ///
    /// Still honoured so an existing `config.toml` keeps reporting the position it
    /// always did, with a note naming the setting's new home.
    pub fixed_location: Option<String>,
}

/// `[location]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocationSection {
    /// `auto`, `fixed`, `gps` or `off`.
    pub mode: Option<PositionMode>,
    /// Fixed coordinates as `"lat,lon"`.
    pub fixed: Option<String>,
    /// A receiver. The header alone is enough to say "this node has one".
    pub gps: Option<GpsSection>,
}

/// `[location.gps]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GpsSection {
    /// `gpsd` or `mock`.
    pub backend: Option<GpsBackend>,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// How long to wait for a report.
    pub timeout_ms: Option<u64>,
    /// How long an acquired fix may be reused.
    pub max_age_ms: Option<u64>,
    /// How long to stop retrying after a failure.
    pub retry_backoff_ms: Option<u64>,
}

/// `[ca]`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaSection {
    /// Where the root signing key lives.
    pub key_path: Option<String>,
    /// Default credential validity in days.
    pub validity_days: Option<u64>,
    /// Default node type for `ca-enroll`: full, light, aggregator or signal.
    pub node_type: Option<String>,
}

impl ConfigFile {
    /// Parse TOML content.
    pub fn parse(contents: &str) -> Result<Self, String> {
        toml::from_str(contents).map_err(|e| format!("Failed to parse configuration: {e}"))
    }

    /// Read and parse a file.
    ///
    /// An absent file is an error: callers only arrive here with a path the
    /// operator named, and a typo'd path must not look like "no defaults
    /// configured".
    pub fn read(path: &std::path::Path) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        Self::parse(&contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_file_leaves_everything_else_unset() {
        let file = ConfigFile::parse("[database]\nurl = \"postgres://localhost/db\"\n").unwrap();

        assert_eq!(
            file.database.unwrap().url.as_deref(),
            Some("postgres://localhost/db")
        );
        assert!(file.log.is_none());
        assert!(file.node.is_none());
        assert!(file.bluetooth.is_none());
        assert!(file.location.is_none());
        assert!(file.ca.is_none());
    }

    #[test]
    fn reads_the_location_and_ca_sections() {
        let file = ConfigFile::parse(
            r#"
[location]
mode = "gps"
fixed = "40.6892,-74.0445"

[location.gps]
backend = "gpsd"
port = 2948

[ca]
key_path = "/etc/btmon/root_key.hex"
validity_days = 90
node_type = "aggregator"
"#,
        )
        .unwrap();

        let location = file.location.unwrap();
        assert_eq!(location.mode, Some(PositionMode::Gps));
        assert_eq!(location.fixed.as_deref(), Some("40.6892,-74.0445"));

        let gps = location.gps.unwrap();
        assert_eq!(gps.backend, Some(GpsBackend::Gpsd));
        assert_eq!(gps.port, Some(2948));
        assert!(
            gps.max_age_ms.is_none(),
            "an omitted setting stays unset so a lower layer can supply it"
        );

        let ca = file.ca.unwrap();
        assert_eq!(ca.key_path.as_deref(), Some("/etc/btmon/root_key.hex"));
        assert_eq!(ca.validity_days, Some(90));
        assert_eq!(ca.node_type.as_deref(), Some("aggregator"));
    }

    #[test]
    fn an_empty_receiver_header_is_stated_but_empty() {
        let file = ConfigFile::parse("[location.gps]\n").unwrap();

        let location = file.location.unwrap();
        assert!(
            location.gps.is_some(),
            "the header itself states a receiver"
        );
        assert!(location.mode.is_none());
        assert_eq!(location.gps.unwrap().host, None);
    }

    #[test]
    fn a_malformed_location_is_a_parse_error_with_the_offending_key() {
        let error = ConfigFile::parse("[location]\nmode = \"wherever\"\n").unwrap_err();

        assert!(
            error.contains("mode"),
            "the message should name the bad key: {error}"
        );
    }

    #[test]
    fn unknown_location_keys_are_rejected_but_unknown_legacy_keys_are_tolerated() {
        // New sections guard against typos; older sections stay permissive so an
        // existing config.toml never stops working after an upgrade.
        assert!(ConfigFile::parse("[location]\nfixd = \"1,2\"\n").is_err());
        assert!(ConfigFile::parse("[location.gps]\nmaxAgeMs = 1\n").is_err());
        assert!(ConfigFile::parse("[database]\nurlx = \"postgres://x\"\n").is_ok());
    }

    #[test]
    fn reading_a_missing_file_is_an_error_naming_it() {
        let error = ConfigFile::read(std::path::Path::new("/nonexistent/node.toml")).unwrap_err();

        assert!(error.contains("node.toml"), "got: {error}");
    }

    #[test]
    fn the_shipped_example_matches_this_schema() {
        // The example is the documentation most operators copy. If a field is
        // renamed here and not there, `deny_unknown_fields` on `[location]` means
        // their node refuses to start, so this has to be checked rather than
        // eyeballed.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../config.example.toml");

        let file = ConfigFile::read(std::path::Path::new(path))
            .unwrap_or_else(|e| panic!("{path} should parse: {e}"));

        let location = file.location.expect("the example shows [location]");
        assert_eq!(location.mode, Some(PositionMode::Auto));
        assert!(location.gps.is_some(), "the example shows a receiver");
        assert!(file.ca.expect("the example shows [ca]").key_path.is_some());
    }
}
