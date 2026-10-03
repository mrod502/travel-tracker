//! Command-line flags, before any layering is applied.
//!
//! Every field is an `Option` and none of them carries a clap `default_value`
//! or `env` attribute. That is the load-bearing rule of the whole design: clap
//! cannot tell "the operator typed `--rate-limit-ms 15000`" apart from "the
//! default filled this in", so a defaulted flag would look like a flag and
//! outrank the config file — inverting the documented precedence. Values that
//! merely *look* like flags are filled by
//! [`resolve`](super::layered::resolve) from the layers below.
//!
//! The same reasoning rules out `#[arg(env = "...")]`: an environment variable
//! belongs to the env layer, where it sits *below* the config file, not to the
//! flag layer where it would sit above it.

/// What the operator actually typed.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    /// `--config-file` / `$CONFIG_FILE`: the TOML file to load.
    ///
    /// The one variable that stays readable by clap, because it names the layer
    /// rather than a value inside it.
    pub config_file: Option<String>,

    // --- Database ---
    pub database_url: Option<String>,
    pub pg_host: Option<String>,
    pub pg_port: Option<u16>,
    pub pg_database: Option<String>,
    pub pg_user: Option<String>,
    pub pg_password: Option<String>,

    // --- Application ---
    pub log_level: Option<String>,
    pub node_id: Option<String>,

    // --- Bluetooth ---
    pub adapter_id: Option<String>,
    pub data_dir: Option<String>,
    pub scan_interval_ms: Option<u64>,
    pub store_raw_payload: Option<bool>,
    pub rate_limit_ms: Option<u64>,
    pub stream_reopen_delay_ms: Option<u64>,
    pub use_mock_backend: Option<bool>,

    // --- Position ---
    /// `--fixed-location`: a shorthand for `[location].fixed`.
    pub fixed_location: Option<String>,
    /// `--location-mode`: `auto`, `fixed`, `gps` or `off`.
    pub location_mode: Option<String>,
    /// `--gps-backend`: `gpsd` or `mock`.
    pub gps_backend: Option<String>,
    pub gps_host: Option<String>,
    pub gps_port: Option<u16>,
    /// How long to wait for a report, the one receiver setting worth flipping
    /// interactively; `max_age` and `retry_backoff` stay in the file or env.
    pub gps_timeout_ms: Option<u64>,
}
