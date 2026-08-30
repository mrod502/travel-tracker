//! The TOML configuration file shape.
//!
//! Every field is optional: the file supplies only what it states, and anything
//! it omits falls through to the env layer or the built-in default. Section names
//! end in `Section` to keep them distinct from the resolved `*Config` structs the
//! application actually reads.

use serde::{Deserialize, Serialize};

use crate::position::PositionConfig;

/// A parsed `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    pub database: Option<DatabaseSection>,
    pub log: Option<LogSection>,
    pub node: Option<NodeSection>,
    pub bluetooth: Option<BluetoothSection>,
    /// Position acquisition settings.
    #[serde(default)]
    pub location: Option<PositionConfig>,
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
    /// Superseded by `[location].fixed`; still honoured so an existing
    /// `config.toml` keeps reporting the position it always did.
    pub fixed_location: Option<String>,
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

    /// Read and parse a file. A file that does not exist is `Ok(None)` only when
    /// it was not explicitly asked for — see [`ConfigFile::load`].
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
        assert_eq!(location.fixed.as_deref(), Some("40.6892,-74.0445"));
        assert_eq!(location.gps.unwrap().port, 2948);

        let ca = file.ca.unwrap();
        assert_eq!(ca.key_path.as_deref(), Some("/etc/btmon/root_key.hex"));
        assert_eq!(ca.validity_days, Some(90));
        assert_eq!(ca.node_type.as_deref(), Some("aggregator"));
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
        assert!(ConfigFile::parse("[database]\nurlx = \"postgres://x\"\n").is_ok());
    }
}
