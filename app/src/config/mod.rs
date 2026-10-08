//! Application configuration.
//!
//! # Precedence
//!
//! A value is taken from the first layer that states it:
//!
//! 1. **Command-line flags** — what the operator typed for this run.
//! 2. **The config file** — named by `--config-file` or `$CONFIG_FILE`, and never
//!    discovered implicitly, so a stray `config.toml` cannot change a node's
//!    behaviour.
//! 3. **The environment** — an exported variable first, then the `.env` file
//!    (`.env`, else `.env.database`, or `ENV_FILE` for an explicit path).
//! 4. **Built-in defaults** in this module.
//!
//! Every value that is not `Option` has a default here, so the layers only ever
//! have to state what differs from a plain node.
//!
//! # Shape
//!
//! Three types describe configuration, and keeping them apart is what makes the
//! precedence rule implementable:
//!
//! * [`Flags`] — what was typed, all `Option`, no defaults.
//! * [`file::ConfigFile`] — the TOML shape, all `Option` `*Section`s.
//! * [`AppConfig`] — the resolved result the application reads.
//!
//! # Validation
//!
//! Validation is per command rather than global. Monitoring needs a database, and
//! checks the shape of a stated node id; `ca init` needs neither. The single
//! `validate()` this replaces made every subcommand demand a node UUID, which is
//! why `ca` and `query` could not run without monitor-shaped configuration.

pub mod env_file;
pub mod file;
pub mod flags;
pub mod layered;

// What callers outside this module name. The layers themselves stay reachable as
// `config::file::ConfigFile` and friends rather than being re-exported here:
// `app` is a bin crate, so a convenience re-export nothing in the binary uses is
// reported as dead code.
pub use flags::Flags;
pub use layered::load;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bt_mon::MonitorConfig;

use crate::node::{Clock, SystemClock};
use crate::position::{build_position_source, PositionError, PositionSource};

pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_PG_HOST: &str = "localhost";
pub const DEFAULT_PG_PORT: u16 = 5432;
pub const DEFAULT_SCAN_INTERVAL_MS: u64 = 1_000;
pub const DEFAULT_RATE_LIMIT_MS: u64 = 15_000;
pub const DEFAULT_STREAM_REOPEN_DELAY_MS: u64 = 1_000;
pub const DEFAULT_CA_KEY_PATH: &str = "/var/lib/btmon/ca/root_key.pem";
pub const DEFAULT_CA_VALIDITY_DAYS: u64 = 90;
pub const DEFAULT_CA_NODE_TYPE: &str = "full";

/// How old the CA's published revocation list may be before this node stops
/// trusting what is *not* in it (one day).
pub const DEFAULT_REVOCATION_MAX_STALENESS_SECS: u64 = 24 * 60 * 60;

/// How often a running node re-reads the published list (fifteen minutes).
///
/// The list's own validity window is measured in days, so re-reading more often
/// than this buys nothing but database queries; re-reading far less often means a
/// revocation takes an unexpectedly long time to reach a node that is already
/// running.
pub const DEFAULT_REVOCATION_REFRESH_SECS: u64 = 15 * 60;

/// Configuration as the application reads it: every layer already collapsed.
///
/// Obtained from [`load`]; a hand-built `AppConfig` is only useful in tests.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// The config file that was read, if any.
    pub config_file: Option<PathBuf>,
    /// The env file that was read, if any.
    pub env_file: Option<PathBuf>,
    pub database: DatabaseConfig,
    pub log: LogConfig,
    pub node: NodeConfig,
    pub bluetooth: BluetoothConfig,
    /// Position acquisition, resolved from `[location]` and the `BT_GPS_*` names.
    pub location: crate::position::PositionConfig,
    /// Defaults for the `ca` subcommands.
    pub ca: CaConfig,
    /// Whether this node consults a CA's revocation list, and which CA it believes.
    pub revocation: RevocationConfig,
}

/// `[database]`: either a DSN or the pieces to build one.
#[derive(Debug, Clone, Default)]
pub struct DatabaseConfig {
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
}

/// `[log]`
#[derive(Debug, Clone)]
pub struct LogConfig {
    pub level: String,
}

/// `[node]`
#[derive(Debug, Clone, Default)]
pub struct NodeConfig {
    /// The identity this node is *expected* to have, as 64 hex characters.
    ///
    /// It does not create an identity — the key pair under `data_dir` does, and
    /// a node id is SHA-256 of its public key. Stated, it becomes an assertion
    /// that startup checks against that key file and refuses to run when it
    /// disagrees, which is what a node about to sign data with the wrong key
    /// needs. Absent means "whatever the key file says", the normal answer for a
    /// node that has never been moved or cloned.
    pub id: Option<String>,
    /// Res 6 H3 cells this node claims authority for, as written by the operator
    /// (hex or decimal). Empty means no claim was stated, which is not the same as
    /// a claim of ownership over nothing.
    ///
    /// Validated when enrollment turns these into cell indices rather than here:
    /// the parser lives beside the geometry it depends on, and a subcommand that
    /// never enrolls should not fail on a stray entry.
    pub owns_cells: Vec<String>,
}

/// `[bluetooth]`
#[derive(Debug, Clone)]
pub struct BluetoothConfig {
    /// Which adapter to open, by id (`hci0`), name, or MAC address.
    ///
    /// Handed to the monitor constructor: selection happens there, because by
    /// the time a monitor exists the adapter is already open. `None` takes the
    /// system's first adapter; a selector that matches nothing — or several —
    /// stops startup with the list of what the machine actually has.
    pub adapter_id: Option<String>,
    /// How often a running scan is re-armed (default: 1000 ms), and the cadence
    /// the simulated radio advertises on under `--use-mock-backend`.
    pub scan_interval_ms: u64,
    /// Whether the undecoded advertisement bytes reach the database.
    ///
    /// On when the node stores whatever the backend's `raw_payload` holds, in
    /// `signal_payload.ble.raw_payload_hex`; off when it drops those bytes before
    /// building the row. The signature covers `signal_payload`, so the flag is
    /// visible to anyone who later checks the record.
    pub store_raw_payload: bool,
    pub data_dir: Option<String>,
    pub rate_limit_ms: u64,
    pub stream_reopen_delay_ms: u64,
    pub use_mock_backend: bool,
}

/// `[ca]`: the defaults the `ca` subcommands fall back on.
#[derive(Debug, Clone)]
pub struct CaConfig {
    pub key_path: String,
    pub validity_days: u64,
    pub node_type: String,
}

/// `[revocation]`: whether this node consults a CA's revocation list at all.
///
/// Off by default, and that default is a decision rather than a placeholder. A
/// node that turns this on stops recording occurrences the moment it can no
/// longer prove the CA's list is current — which is the point of revocation, and
/// also what it feels like the first time a CA is offline for maintenance. An
/// operator should opt into that trade knowingly, and `[revocation]` is where
/// they say so.
///
/// Enabling it makes the node useless without the other two settings, so
/// [`validate_monitor`](Self::validate_monitor) refuses the combination at
/// startup rather than letting it run and quietly store nothing.
#[derive(Debug, Clone)]
pub struct RevocationConfig {
    pub enabled: bool,
    /// The CA whose lists this node believes, as an SPKI PEM file — the file
    /// `ca ca-export-anchor` writes. Required when `enabled`: a list with no key
    /// to check it against is a document anybody could have written.
    pub anchor_path: Option<String>,
    /// How old the CA's list may be before this node stops treating a node's
    /// absence from it as "not revoked".
    pub max_staleness_secs: u64,
    /// How often a running node re-reads the published list.
    pub refresh_secs: u64,
}

impl Default for RevocationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            anchor_path: None,
            max_staleness_secs: DEFAULT_REVOCATION_MAX_STALENESS_SECS,
            refresh_secs: DEFAULT_REVOCATION_REFRESH_SECS,
        }
    }
}

impl AppConfig {
    /// Check what a monitoring session needs: a usable identity assertion, a
    /// reachable-looking database, sane intervals, and a position setup that can
    /// actually be built.
    pub fn validate_monitor(&self) -> Result<(), String> {
        // The identity itself comes from the key file, so saying nothing about it
        // is not an error. Saying something malformed is, because the assertion
        // the operator meant to make would silently not be made.
        self.expected_node_id()?;

        self.validate_database()?;

        if self.bluetooth.scan_interval_ms == 0 {
            return Err("scan interval must be greater than 0".to_string());
        }
        if self.bluetooth.rate_limit_ms == 0 {
            return Err("rate limit must be greater than 0".to_string());
        }
        // The event stream is reopened after this delay, so 0 would spin.
        if self.bluetooth.stream_reopen_delay_ms == 0 {
            return Err("stream reopen delay must be greater than 0".to_string());
        }

        // Building is cheap and connects to nothing, so a mode that cannot be
        // satisfied (`mode = "fixed"` with no fixed point) fails here rather than
        // silently storing NULL locations forever.
        self.position_source()
            .map_err(|e| format!("invalid location configuration: {e}"))?;

        self.validate_revocation()?;

        Ok(())
    }

    /// Check `[revocation]` says something a node can act on.
    ///
    /// Only the shape is checkable here — whether the anchor file exists and holds
    /// a key, and whether its CA has published anything, needs the database, and
    /// belongs to the node's startup.
    pub fn validate_revocation(&self) -> Result<(), String> {
        if !self.revocation.enabled {
            return Ok(());
        }

        match self.revocation.anchor_path.as_deref().map(str::trim) {
            None | Some("") => {
                return Err("[revocation].enabled is set without a trust anchor: give \
                     [revocation].anchor_path (or REVOCATION_ANCHOR_PATH) the SPKI PEM file \
                     `ca ca-export-anchor` writes. A revocation list with no key to check it \
                     against is a document anybody could have written, so there is nothing this \
                     node could believe."
                    .to_string());
            }
            Some(_) => {}
        }

        if self.revocation.max_staleness_secs == 0 {
            return Err(
                "[revocation].max_staleness_secs must be greater than 0 — a bound of zero makes \
                 every list stale the moment it is read, and the node would record nothing."
                    .to_string(),
            );
        }

        // The refresh loop re-arms after this delay, so 0 would spin against the
        // database.
        if self.revocation.refresh_secs == 0 {
            return Err("[revocation].refresh_secs must be greater than 0".to_string());
        }

        Ok(())
    }

    /// Check what `query`, `stats` and `ca enroll` need. Deliberately does not
    /// require a node id: those commands act on the database, not as a node.
    pub fn validate_database(&self) -> Result<(), String> {
        match self.database.is_configured() {
            true => Ok(()),
            false => Err(
                "no database configured: set --database-url, DATABASE_URL, or \
                 [database].url — or the [database].database and [database].user pair"
                    .to_string(),
            ),
        }
    }

    /// The connection string, from the DSN or built from the components.
    pub fn database_url(&self) -> Result<String, String> {
        self.database.url()
    }

    /// The connection string when one is configured, without an error message.
    ///
    /// For call sites that have their own, more specific message.
    pub fn database_url_opt(&self) -> Option<String> {
        match self.database.is_configured() {
            true => self.database.url().ok(),
            false => None,
        }
    }

    /// The node identity the configuration asserts, when it asserts one.
    ///
    /// `Ok(None)` means the operator named no identity and the key file under
    /// [`data_dir`](Self::data_dir) decides. `Ok(Some(bytes))` is checked against
    /// that key file before the node signs anything, so a stale or copied `id`
    /// stops startup instead of labelling a month of rows with the wrong node.
    pub fn expected_node_id(&self) -> Result<Option<Vec<u8>>, String> {
        self.node.id.as_deref().map(parse_node_id).transpose()
    }

    /// Where node identity and state files live.
    pub fn data_dir(&self) -> Result<PathBuf, String> {
        self.bluetooth.data_dir()
    }

    /// The configured position chain, ready to query.
    ///
    /// Connects to nothing: a gpsd source only opens a socket on the first
    /// `current_position()`, so calling this at startup validates the
    /// configuration rather than the receiver.
    pub fn position_source(&self) -> Result<Arc<dyn PositionSource>, PositionError> {
        self.position_source_with_clock(Arc::new(SystemClock))
    }

    /// [`position_source`](Self::position_source) with the clock that timestamps
    /// simulated fixes and ages cached ones.
    pub fn position_source_with_clock(
        &self,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<dyn PositionSource>, PositionError> {
        build_position_source(&self.location, clock)
    }

    /// Log-friendly summary. Secrets are reported as present-or-absent, never
    /// echoed, because this line ends up in journald.
    pub fn pretty_print(&self) -> String {
        #[derive(serde::Serialize)]
        struct Out<'a> {
            config_file: Option<&'a str>,
            env_file: Option<&'a str>,
            database: DatabaseOut<'a>,
            logging: LogOut<'a>,
            node: NodeOut<'a>,
            bluetooth: BluetoothOut<'a>,
            location: LocationOut<'a>,
            ca: CaOut<'a>,
        }

        #[derive(serde::Serialize)]
        struct DatabaseOut<'a> {
            url_set: bool,
            host: Option<&'a str>,
            port: Option<u16>,
            database: Option<&'a str>,
            user: Option<&'a str>,
            password_set: bool,
        }

        #[derive(serde::Serialize)]
        struct LogOut<'a> {
            level: &'a str,
        }

        #[derive(serde::Serialize)]
        struct NodeOut<'a> {
            /// The identity startup will check the key file against, or `null`.
            ///
            /// Echoed rather than reduced to a boolean: it is a public hash, and
            /// an operator comparing two machines has to see which node each one
            /// claims to be.
            expected_id: Option<&'a str>,
            owns_cells: &'a [String],
        }

        #[derive(serde::Serialize)]
        struct BluetoothOut<'a> {
            scan_interval_ms: u64,
            store_raw_payload: bool,
            adapter_id: Option<&'a str>,
            data_dir: Option<&'a str>,
            rate_limit_ms: u64,
            stream_reopen_delay_ms: u64,
            use_mock_backend: bool,
        }

        #[derive(serde::Serialize)]
        struct LocationOut<'a> {
            mode: &'a str,
            fixed: Option<&'a str>,
            gps_backend: Option<&'a str>,
            gps_endpoint: Option<String>,
        }

        #[derive(serde::Serialize)]
        struct CaOut<'a> {
            key_path: &'a str,
            validity_days: u64,
            node_type: &'a str,
        }

        let gps = self.location.gps.as_ref();
        let out = Out {
            config_file: self.config_file.as_deref().and_then(|p| p.to_str()),
            env_file: self.env_file.as_deref().and_then(|p| p.to_str()),
            database: DatabaseOut {
                url_set: self.database.url.as_deref().is_some_and(|u| !u.is_empty()),
                host: self.database.host.as_deref(),
                port: self.database.port,
                database: self.database.database.as_deref(),
                user: self.database.user.as_deref(),
                password_set: self
                    .database
                    .password
                    .as_deref()
                    .is_some_and(|p| !p.is_empty()),
            },
            logging: LogOut {
                level: &self.log.level,
            },
            node: NodeOut {
                expected_id: self.node.id.as_deref(),
                owns_cells: &self.node.owns_cells,
            },
            bluetooth: BluetoothOut {
                scan_interval_ms: self.bluetooth.scan_interval_ms,
                store_raw_payload: self.bluetooth.store_raw_payload,
                adapter_id: self.bluetooth.adapter_id.as_deref(),
                data_dir: self.bluetooth.data_dir.as_deref(),
                rate_limit_ms: self.bluetooth.rate_limit_ms,
                stream_reopen_delay_ms: self.bluetooth.stream_reopen_delay_ms,
                use_mock_backend: self.bluetooth.use_mock_backend,
            },
            location: LocationOut {
                mode: mode_label(self.location.mode),
                fixed: self.location.fixed.as_deref(),
                gps_backend: gps.map(|g| backend_label(g.backend)),
                gps_endpoint: gps.map(|g| format!("{}:{}", g.host, g.port)),
            },
            ca: CaOut {
                key_path: &self.ca.key_path,
                validity_days: self.ca.validity_days,
                node_type: &self.ca.node_type,
            },
        };

        serde_json::to_string_pretty(&out)
            .unwrap_or_else(|_| "failed to serialize configuration".to_string())
    }
}

impl DatabaseConfig {
    /// Whether a connection could be attempted at all.
    pub fn is_configured(&self) -> bool {
        self.has_dsn() || (self.database.is_some() && self.user.is_some())
    }

    fn has_dsn(&self) -> bool {
        self.url.as_deref().is_some_and(|url| !url.is_empty())
    }

    /// The DSN, or one assembled from the components.
    pub fn url(&self) -> Result<String, String> {
        if self.has_dsn() {
            return Ok(self.url.clone().unwrap_or_default());
        }

        let database = self
            .database
            .as_deref()
            .ok_or("no database name: set --pg-database, PGDATABASE, or [database].database")?;
        let user = self
            .user
            .as_deref()
            .ok_or("no database user: set --pg-user, PGUSER, or [database].user")?;
        let password = self.password.as_deref().unwrap_or("");

        Ok(format!(
            "postgres://{}:{}@{}:{}/{}",
            user,
            escape_conn_str(password),
            self.host.as_deref().unwrap_or(DEFAULT_PG_HOST),
            self.port.unwrap_or(DEFAULT_PG_PORT),
            database
        ))
    }
}

impl BluetoothConfig {
    /// Where node identity and state files live: the configured directory, or
    /// `$HOME/.btmon/data`.
    pub fn data_dir(&self) -> Result<PathBuf, String> {
        match self.data_dir.as_deref() {
            Some(dir) => Ok(PathBuf::from(dir)),
            None => default_data_dir(),
        }
    }

    /// The adapter selection and scan cadence, in the form the monitor's
    /// constructor takes them.
    ///
    /// Both settings are constructor arguments: by the time a monitor exists the
    /// adapter is already chosen and the scan already configured, so anything
    /// that arrives later is printed at the operator and then dropped. Handing
    /// them over as one value is what keeps that boundary crossing testable.
    pub fn monitor_config(&self) -> MonitorConfig {
        let config =
            MonitorConfig::new().with_scan_interval(Duration::from_millis(self.scan_interval_ms));

        match self
            .adapter_id
            .as_deref()
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
        {
            Some(selector) => config.with_adapter(selector),
            None => config,
        }
    }
}

/// A node id is the 32 raw bytes of SHA-256(signing public key), printed as
/// this many hex characters.
pub const NODE_ID_HEX_LEN: usize = 64;

/// Parse a node identity written by an operator.
///
/// Accepts the form the node prints about itself — 64 hex characters, either
/// case, an optional `0x` prefix — and nothing else. A UUID in particular is
/// refused by name: it is what this setting used to ask for, it cannot hold 32
/// bytes, and an operator who copies an old `NODE_ID` deserves to be told which
/// of the two spellings is the current one rather than to be told the string is
/// malformed.
pub fn parse_node_id(raw: &str) -> Result<Vec<u8>, String> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    if body.len() != NODE_ID_HEX_LEN {
        let hint = if looks_like_uuid(body) {
            " — a UUID is not a node id: a node id is SHA-256 of the node's \
             Ed25519 public key, printed as 64 hex characters by `ca ca-enroll` \
             and in the startup log"
        } else {
            ""
        };
        return Err(format!(
            "invalid node id '{raw}': expected {NODE_ID_HEX_LEN} hex characters \
             (32 bytes){hint}"
        ));
    }

    hex::decode(body).map_err(|e| format!("invalid node id '{raw}': {e}"))
}

/// Whether the rejected value is a UUID, so the error can say what to use instead.
fn looks_like_uuid(raw: &str) -> bool {
    raw.len() == 36 && raw.matches('-').count() == 4
}

/// `$HOME/.btmon/data`.
///
/// Derived at call time rather than baked in at compile time so the binary works
/// for whichever user runs it.
pub fn default_data_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| {
        "HOME is not set, so the default data directory cannot be derived; set --data-dir, \
         BT_DATA_DIR, or [bluetooth].data_dir"
    })?;

    Ok(PathBuf::from(home).join(DEFAULT_DATA_DIR))
}

/// The data directory relative to `$HOME`.
const DEFAULT_DATA_DIR: &str = ".btmon/data";

/// Escape characters that would otherwise end or confuse a connection-string
/// value.
fn escape_conn_str(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "'\\''")
}

fn mode_label(mode: crate::position::PositionMode) -> &'static str {
    use crate::position::PositionMode;
    match mode {
        PositionMode::Auto => "auto",
        PositionMode::Fixed => "fixed",
        PositionMode::Gps => "gps",
        PositionMode::Off => "off",
    }
}

fn backend_label(backend: crate::position::GpsBackend) -> &'static str {
    use crate::position::GpsBackend;
    match backend {
        GpsBackend::Gpsd => "gpsd",
        GpsBackend::Mock => "mock",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::PositionMode;

    /// A node id in the shape the node prints about itself: 32 bytes, hex.
    const NODE_ID_HEX: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

    fn config() -> AppConfig {
        AppConfig {
            config_file: None,
            env_file: None,
            database: DatabaseConfig {
                url: Some("postgres://localhost/test".to_string()),
                host: None,
                port: None,
                database: None,
                user: None,
                password: None,
            },
            log: LogConfig {
                level: DEFAULT_LOG_LEVEL.to_string(),
            },
            node: NodeConfig {
                id: Some(NODE_ID_HEX.to_string()),
                owns_cells: Vec::new(),
            },
            bluetooth: BluetoothConfig {
                adapter_id: None,
                scan_interval_ms: DEFAULT_SCAN_INTERVAL_MS,
                store_raw_payload: true,
                data_dir: None,
                rate_limit_ms: DEFAULT_RATE_LIMIT_MS,
                stream_reopen_delay_ms: DEFAULT_STREAM_REOPEN_DELAY_MS,
                use_mock_backend: false,
            },
            location: Default::default(),
            ca: CaConfig {
                key_path: DEFAULT_CA_KEY_PATH.to_string(),
                validity_days: DEFAULT_CA_VALIDITY_DAYS,
                node_type: DEFAULT_CA_NODE_TYPE.to_string(),
            },
            revocation: RevocationConfig::default(),
        }
    }

    #[test]
    fn a_plain_configuration_passes_monitor_validation() {
        assert_eq!(config().validate_monitor(), Ok(()));
    }

    /// Revocation switched on with nothing to check a list against is not a node
    /// with looser rules — it is a node that will never record anything, because
    /// every status is unknown and the policies refuse to guess. That is worth a
    /// startup error rather than a week of an empty database.
    #[test]
    fn revocation_without_a_trust_anchor_is_a_startup_error() {
        let mut config = config();
        config.revocation.enabled = true;

        let error = config
            .validate_monitor()
            .expect_err("no anchor was configured");
        assert!(error.contains("anchor_path"), "{error}");

        config.revocation.anchor_path = Some("/etc/btmon/ca.pub.pem".to_string());
        assert_eq!(config.validate_monitor(), Ok(()));
    }

    #[test]
    fn intervals_that_would_make_revocation_checking_absurd_are_refused() {
        let mut config = config();
        config.revocation = RevocationConfig {
            enabled: true,
            anchor_path: Some("/etc/btmon/ca.pub.pem".to_string()),
            max_staleness_secs: 0,
            refresh_secs: 60,
        };

        let error = config
            .validate_revocation()
            .expect_err("a zero bound means every list is stale on arrival");
        assert!(error.contains("max_staleness_secs"), "{error}");

        config.revocation.max_staleness_secs = 3600;
        config.revocation.refresh_secs = 0;
        let error = config
            .validate_revocation()
            .expect_err("a zero refresh delay would spin against the database");
        assert!(error.contains("refresh_secs"), "{error}");
    }

    #[test]
    fn revocation_settings_are_not_checked_while_switched_off() {
        let mut config = config();
        config.revocation = RevocationConfig {
            enabled: false,
            anchor_path: None,
            max_staleness_secs: 0,
            refresh_secs: 0,
        };

        assert_eq!(config.validate_revocation(), Ok(()));
    }

    #[test]
    fn a_node_id_asserts_an_identity_rather_than_supplying_one() {
        // The key file is the identity; `node.id` only checks it. So a run with
        // nothing stated is a normal run, not a missing setting.
        let mut config = config();
        config.node.id = None;

        assert_eq!(config.validate_monitor(), Ok(()));
        assert_eq!(config.expected_node_id().unwrap(), None);
        assert_eq!(config.validate_database(), Ok(()));
    }

    #[test]
    fn a_stated_node_id_is_read_as_the_bytes_the_node_reports_about_itself() {
        let bytes = config().expected_node_id().unwrap().unwrap();

        assert_eq!(bytes.len(), 32, "a node id is SHA-256, so 32 raw bytes");
        assert_eq!(&bytes[..2], &[0x0f, 0x1e]);
        assert_eq!(&bytes[30..], &[0xe1, 0xf0]);
    }

    #[test]
    fn a_node_id_is_accepted_upper_case_padded_or_prefixed() {
        let upper = NODE_ID_HEX.to_uppercase();

        for stated in [
            NODE_ID_HEX.to_string(),
            upper,
            format!("  {NODE_ID_HEX}  "),
            format!("0x{NODE_ID_HEX}"),
        ] {
            assert_eq!(
                parse_node_id(&stated).unwrap().len(),
                32,
                "'{stated}' is a node id the node itself would print"
            );
        }
    }

    #[test]
    fn a_uuid_is_refused_by_name_because_it_cannot_hold_a_node_id() {
        // This setting used to ask for a UUID, so the old spelling is the likeliest
        // thing to arrive here; "malformed" alone would leave the operator
        // re-supplying the same unusable value.
        let error = parse_node_id("550e8400-e29b-41d4-a716-446655440000").unwrap_err();

        assert!(error.contains("UUID"), "got: {error}");
        assert!(error.contains("64 hex"), "got: {error}");
    }

    #[test]
    fn a_malformed_node_id_is_reported_with_the_value_that_failed() {
        let mut config = config();
        config.node.id = Some("not-a-uuid".to_string());

        let error = config.validate_monitor().unwrap_err();
        assert!(error.contains("not-a-uuid"), "got: {error}");
    }

    #[test]
    fn a_database_needs_either_a_dsn_or_a_name_and_user() {
        let mut config = config();
        config.database = DatabaseConfig::default();
        assert!(config.validate_database().unwrap_err().contains("database"));

        config.database.database = Some("travel".to_string());
        assert!(
            config.validate_database().is_err(),
            "a name without a user is not a configuration"
        );

        config.database.user = Some("btmon".to_string());
        assert_eq!(config.validate_database(), Ok(()));
    }

    #[test]
    fn a_dsn_wins_over_the_components_it_contradicts() {
        let mut config = config();
        config.database = DatabaseConfig {
            url: Some("postgres://custom@elsewhere:9999/custom".to_string()),
            host: Some("localhost".to_string()),
            port: Some(5432),
            database: Some("other".to_string()),
            user: Some("other".to_string()),
            password: Some("other".to_string()),
        };

        assert_eq!(
            config.database_url().unwrap(),
            "postgres://custom@elsewhere:9999/custom"
        );
    }

    #[test]
    fn a_connection_string_is_assembled_from_the_components() {
        let mut config = config();
        config.database = DatabaseConfig {
            url: None,
            host: Some("myhost".to_string()),
            port: Some(1234),
            database: Some("mydb".to_string()),
            user: Some("myuser".to_string()),
            password: Some("mypass".to_string()),
        };

        assert_eq!(
            config.database_url().unwrap(),
            "postgres://myuser:mypass@myhost:1234/mydb"
        );
    }

    #[test]
    fn missing_connection_components_fall_back_to_the_documented_defaults() {
        let mut config = config();
        config.database = DatabaseConfig {
            url: None,
            host: None,
            port: None,
            database: Some("mydb".to_string()),
            user: Some("myuser".to_string()),
            password: None,
        };

        assert_eq!(
            config.database_url().unwrap(),
            "postgres://myuser:@localhost:5432/mydb"
        );
    }

    #[test]
    fn an_unconfigured_database_url_names_the_settings_that_would_supply_it() {
        let config = AppConfig {
            database: DatabaseConfig::default(),
            ..config()
        };

        let error = config.database_url().unwrap_err();
        assert!(error.contains("--pg-database"), "got: {error}");
    }

    #[test]
    fn database_url_opt_is_quiet_about_the_absent_case() {
        assert_eq!(
            config().database_url_opt().as_deref(),
            Some("postgres://localhost/test")
        );
        assert_eq!(
            AppConfig {
                database: DatabaseConfig::default(),
                ..config()
            }
            .database_url_opt(),
            None
        );
    }

    #[test]
    fn zero_intervals_are_rejected_with_a_reason() {
        for mutate in [
            |c: &mut AppConfig| c.bluetooth.scan_interval_ms = 0,
            |c: &mut AppConfig| c.bluetooth.rate_limit_ms = 0,
            |c: &mut AppConfig| c.bluetooth.stream_reopen_delay_ms = 0,
        ] {
            let mut config = config();
            mutate(&mut config);

            assert!(
                config.validate_monitor().is_err(),
                "{config:?} should not validate"
            );
        }
    }

    #[test]
    fn a_data_directory_of_none_becomes_home_btmon_data() {
        if std::env::var("HOME").is_err() {
            return;
        }

        let path = config().data_dir().unwrap();
        assert!(path.ends_with(".btmon/data"), "got: {}", path.display());
    }

    #[test]
    fn a_configured_data_directory_is_used_verbatim() {
        let mut config = config();
        config.bluetooth.data_dir = Some("/var/lib/btmon".to_string());

        assert_eq!(config.data_dir().unwrap(), PathBuf::from("/var/lib/btmon"));
    }

    #[test]
    fn validation_catches_a_location_mode_that_cannot_be_built() {
        let mut config = config();
        config.location = crate::position::PositionConfig {
            mode: PositionMode::Fixed,
            fixed: None,
            gps: None,
        };

        let error = config.validate_monitor().unwrap_err();
        assert!(
            error.contains("fixed"),
            "the message should name the missing piece: {error}"
        );
        // The database commands must not be held hostage by the monitor's rules,
        // but a broken location is still broken for `ca enroll`.
        assert_eq!(config.validate_database(), Ok(()));
    }

    #[test]
    fn a_fixed_location_resolves_to_a_usable_source() {
        let mut config = config();
        config.location = crate::position::PositionConfig {
            mode: PositionMode::Fixed,
            fixed: Some("40.6892,-74.0445".to_string()),
            gps: None,
        };

        assert_eq!(config.validate_monitor(), Ok(()));
        assert!(config.position_source().is_ok());
    }

    #[test]
    fn the_pretty_print_never_echoes_the_password() {
        let mut config = config();
        config.database.password = Some("hunter2".to_string());
        config.database.url = None;
        config.database.database = Some("travel".to_string());
        config.database.user = Some("btmon".to_string());

        let printed = config.pretty_print();

        assert!(!printed.contains("hunter2"), "the secret leaked: {printed}");
        assert!(printed.contains("password_set"), "got: {printed}");
        assert!(
            printed.contains("\"mode\""),
            "location should be reported: {printed}"
        );
        assert!(
            printed.contains("key_path"),
            "ca should be reported: {printed}"
        );
    }

    #[test]
    fn the_pretty_print_reports_the_identity_the_node_will_check() {
        let printed = config().pretty_print();

        assert!(
            printed.contains(NODE_ID_HEX),
            "the expected id is what an operator compares against the key file: {printed}"
        );

        let mut unnamed = config();
        unnamed.node.id = None;
        let printed = unnamed.pretty_print();

        assert!(
            printed.contains("\"expected_id\": null"),
            "an identity that will not be checked has to read as unchecked: {printed}"
        );
    }
}
