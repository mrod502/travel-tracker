//! Turning the three configuration layers into one resolved [`AppConfig`].
//!
//! A value is taken from the first layer that *states* it:
//!
//! ```text
//! command-line flag  >  config file  >  environment (process env, then .env)  >  default
//! ```
//!
//! "States" is the whole trick, and it is why [`Flags`](super::Flags) carries no
//! defaults: `--rate-limit-ms 15000` and an omitted flag have to stay
//! distinguishable, or a flag default silently outranks the file. The version
//! this replaces compared values against their defaults
//! (`if self.rate_limit_ms == 15000 { … }`), so an operator who explicitly asked
//! for 15000 lost it to the file.
//!
//! Every field is resolved in one pass and *all* problems are reported together,
//! because an operator who has to run a command four times to find four typos in
//! one file will not trust the fourth run either.

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::position::{GpsBackend, GpsSettings, PositionConfig, PositionMode};

use super::env_file::EnvLayer;
use super::file::{BluetoothSection, ConfigFile, GpsSection, LocationSection};
use super::flags::Flags;
use super::{
    AppConfig, BluetoothConfig, CaConfig, DatabaseConfig, LogConfig, NodeConfig,
    DEFAULT_CA_KEY_PATH, DEFAULT_CA_NODE_TYPE, DEFAULT_CA_VALIDITY_DAYS, DEFAULT_LOG_LEVEL,
    DEFAULT_RATE_LIMIT_MS, DEFAULT_SCAN_INTERVAL_MS, DEFAULT_STREAM_REOPEN_DELAY_MS,
};

/// Environment variables read by the env layer.
///
/// These are the names the application has always used. [`FIXED_LOCATION`] is
/// the exception: a deprecated alias of `[location].fixed` that is still
/// honoured, with a note.
pub mod env_keys {
    pub const DATABASE_URL: &str = "DATABASE_URL";
    pub const PGHOST: &str = "PGHOST";
    pub const PGPORT: &str = "PGPORT";
    pub const PGDATABASE: &str = "PGDATABASE";
    pub const PGUSER: &str = "PGUSER";
    pub const PGPASSWORD: &str = "PGPASSWORD";

    pub const LOG_LEVEL: &str = "LOG_LEVEL";
    pub const NODE_ID: &str = "NODE_ID";
    /// Comma-separated res 6 cells, the env spelling of `[node].owns_cells`.
    pub const NODE_OWNS_CELLS: &str = "BT_OWNS_CELLS";

    pub const BT_ADAPTER_ID: &str = "BT_ADAPTER_ID";
    pub const BT_DATA_DIR: &str = "BT_DATA_DIR";
    pub const BT_SCAN_INTERVAL_MS: &str = "BT_SCAN_INTERVAL_MS";
    pub const BT_STORE_RAW_PAYLOAD: &str = "BT_STORE_RAW_PAYLOAD";
    pub const BT_RATE_LIMIT_MS: &str = "BT_RATE_LIMIT_MS";
    pub const BT_STREAM_REOPEN_DELAY_MS: &str = "BT_STREAM_REOPEN_DELAY_MS";
    pub const BT_USE_MOCK_BACKEND: &str = "BT_USE_MOCK_BACKEND";

    pub const LOCATION_FIXED: &str = "BT_LOCATION_FIXED";
    /// Deprecated spelling of [`LOCATION_FIXED`].
    pub const FIXED_LOCATION: &str = "BT_FIXED_LOCATION";
    pub const LOCATION_MODE: &str = "BT_LOCATION_MODE";
    pub const GPS_BACKEND: &str = "BT_GPS_BACKEND";
    pub const GPS_HOST: &str = "BT_GPS_HOST";
    pub const GPS_PORT: &str = "BT_GPS_PORT";
    pub const GPS_TIMEOUT_MS: &str = "BT_GPS_TIMEOUT_MS";
    pub const GPS_MAX_AGE_MS: &str = "BT_GPS_MAX_AGE_MS";
    pub const GPS_RETRY_BACKOFF_MS: &str = "BT_GPS_RETRY_BACKOFF_MS";

    pub const CA_ROOT_KEY_PATH: &str = "CA_ROOT_KEY_PATH";
    pub const CA_VALIDITY_DAYS: &str = "CA_VALIDITY_DAYS";
    pub const CA_NODE_TYPE: &str = "CA_NODE_TYPE";
}

/// A resolved configuration plus anything the operator should be told about how
/// its values were arrived at.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub config: AppConfig,
    /// Deprecations and other notes, for logging once the logger exists.
    pub notes: Vec<String>,
}

/// Resolve the configuration for `flags`, reading the layers it points at.
///
/// The config file is only read when `--config-file` (or `$CONFIG_FILE`) names
/// one; nothing is discovered implicitly, so an unrelated `config.toml` in the
/// working directory cannot change how a node behaves. A named file that cannot
/// be read is an error rather than a warning — a typo'd path otherwise looks
/// exactly like "no defaults configured".
pub fn load(flags: &Flags) -> Result<Resolved, Vec<String>> {
    load_from(flags, Path::new("."))
}

/// [`load`] with the `.env` search directory supplied, for tests.
pub fn load_from(flags: &Flags, base_dir: &Path) -> Result<Resolved, Vec<String>> {
    let env = EnvLayer::discover(base_dir).map_err(|e| vec![e])?;

    let file = match flags.config_file.as_deref() {
        Some(path) => ConfigFile::read(Path::new(path)).map_err(|e| vec![e])?,
        None => ConfigFile::default(),
    };

    let mut resolved = resolve(flags, &file, &env)?;
    resolved.config.config_file = flags.config_file.as_deref().map(PathBuf::from);
    resolved.config.env_file = env.file_path().map(Path::to_path_buf);

    Ok(resolved)
}

/// Apply the precedence rule to layers that are already in hand.
pub fn resolve(flags: &Flags, file: &ConfigFile, env: &EnvLayer) -> Result<Resolved, Vec<String>> {
    let mut resolver = Resolver::new(env);
    let config = resolver.app_config(flags, file);

    if resolver.errors.is_empty() {
        Ok(Resolved {
            config,
            notes: resolver.notes,
        })
    } else {
        Err(resolver.errors)
    }
}

/// Walks the layers for one resolution, collecting errors instead of aborting on
/// the first one.
struct Resolver<'a> {
    env: &'a EnvLayer,
    errors: Vec<String>,
    notes: Vec<String>,
}

impl<'a> Resolver<'a> {
    fn new(env: &'a EnvLayer) -> Self {
        Self {
            env,
            errors: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Text: the first layer that states a value wins.
    fn text(&self, flag: Option<String>, file: Option<String>, key: &str) -> Option<String> {
        flag.or(file).or_else(|| self.env.get(key))
    }

    /// A list of strings. The env has no list type, so `BT_OWNS_CELLS` spells one
    /// out with commas; blank entries are dropped rather than reported, since
    /// `A,,B` and a trailing comma are how a shell-built list looks.
    fn text_list(
        &self,
        flag: Option<Vec<String>>,
        file: Option<Vec<String>>,
        key: &str,
    ) -> Vec<String> {
        let stated = flag.or(file).or_else(|| {
            self.env
                .get(key)
                .map(|raw| raw.split(',').map(str::to_string).collect())
        });

        stated
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.trim().to_string())
            .filter(|entry| !entry.is_empty())
            .collect()
    }

    /// A number. Only the env layer can arrive malformed: clap parses the flags
    /// and serde parses the file, both before this point.
    fn number<T>(&mut self, flag: Option<T>, file: Option<T>, key: &str) -> Option<T>
    where
        T: Copy + FromStr,
        T::Err: Display,
    {
        if flag.is_some() {
            return flag;
        }
        if file.is_some() {
            return file;
        }
        self.env_value(key, |raw| {
            raw.parse::<T>()
                .map_err(|e| format!("'{raw}' is not a number ({e})"))
        })
    }

    /// A boolean, spelled `true`/`false` in the file and loosely in the env.
    fn boolean(&mut self, flag: Option<bool>, file: Option<bool>, key: &str) -> Option<bool> {
        if flag.is_some() {
            return flag;
        }
        if file.is_some() {
            return file;
        }
        self.env_value(key, |raw| {
            parse_bool(raw).ok_or_else(|| format!("'{raw}' is not a boolean (use true or false)"))
        })
    }

    /// Read one env variable through a parser, reporting a bad value against the
    /// variable it came from.
    fn env_value<T>(&mut self, key: &str, parse: impl Fn(&str) -> Result<T, String>) -> Option<T> {
        let raw = self.env.get(key)?;

        match parse(&raw) {
            Ok(value) => Some(value),
            Err(e) => {
                self.errors.push(format!("{key}: {e}"));
                None
            }
        }
    }

    /// Parse an enumerated value from one layer, blaming that layer by name.
    fn enumerated<T>(
        &mut self,
        raw: Option<&str>,
        source: &str,
        parse: impl Fn(&str) -> Result<T, String>,
    ) -> Option<T> {
        let Some(raw) = raw else { return None };

        match parse(raw) {
            Ok(value) => Some(value),
            Err(e) => {
                self.errors.push(format!("{source}: {e}"));
                None
            }
        }
    }

    fn app_config(&mut self, flags: &Flags, file: &ConfigFile) -> AppConfig {
        let database = file.database.as_ref();
        let bluetooth = file.bluetooth.as_ref();

        let config = AppConfig {
            config_file: None,
            env_file: None,
            database: DatabaseConfig {
                url: self.text(
                    flags.database_url.clone(),
                    database.and_then(|d| d.url.clone()),
                    env_keys::DATABASE_URL,
                ),
                host: self.text(
                    flags.pg_host.clone(),
                    database.and_then(|d| d.host.clone()),
                    env_keys::PGHOST,
                ),
                port: self.number(
                    flags.pg_port,
                    database.and_then(|d| d.port),
                    env_keys::PGPORT,
                ),
                database: self.text(
                    flags.pg_database.clone(),
                    database.and_then(|d| d.database.clone()),
                    env_keys::PGDATABASE,
                ),
                user: self.text(
                    flags.pg_user.clone(),
                    database.and_then(|d| d.user.clone()),
                    env_keys::PGUSER,
                ),
                password: self.text(
                    flags.pg_password.clone(),
                    database.and_then(|d| d.password.clone()),
                    env_keys::PGPASSWORD,
                ),
            },
            log: LogConfig {
                level: self
                    .text(
                        flags.log_level.clone(),
                        file.log.as_ref().and_then(|l| l.level.clone()),
                        env_keys::LOG_LEVEL,
                    )
                    .unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_string()),
            },
            node: NodeConfig {
                id: self.text(
                    flags.node_id.clone(),
                    file.node.as_ref().and_then(|n| n.id.clone()),
                    env_keys::NODE_ID,
                ),
                // No flag layer: `--owns-cell` belongs to `ca ca-enroll` rather than
                // to the global flags, so enrollment merges it in front of this.
                owns_cells: self.text_list(
                    None,
                    file.node.as_ref().and_then(|n| n.owns_cells.clone()),
                    env_keys::NODE_OWNS_CELLS,
                ),
            },
            bluetooth: BluetoothConfig {
                adapter_id: self.text(
                    flags.adapter_id.clone(),
                    bluetooth.and_then(|b| b.adapter_id.clone()),
                    env_keys::BT_ADAPTER_ID,
                ),
                scan_interval_ms: self
                    .number(
                        flags.scan_interval_ms,
                        bluetooth.and_then(|b| b.scan_interval_ms),
                        env_keys::BT_SCAN_INTERVAL_MS,
                    )
                    .unwrap_or(DEFAULT_SCAN_INTERVAL_MS),
                store_raw_payload: self
                    .boolean(
                        flags.store_raw_payload,
                        bluetooth.and_then(|b| b.store_raw_payload),
                        env_keys::BT_STORE_RAW_PAYLOAD,
                    )
                    .unwrap_or(true),
                data_dir: self.text(
                    flags.data_dir.clone(),
                    bluetooth.and_then(|b| b.data_dir.clone()),
                    env_keys::BT_DATA_DIR,
                ),
                rate_limit_ms: self
                    .number(
                        flags.rate_limit_ms,
                        bluetooth.and_then(|b| b.rate_limit_ms),
                        env_keys::BT_RATE_LIMIT_MS,
                    )
                    .unwrap_or(DEFAULT_RATE_LIMIT_MS),
                stream_reopen_delay_ms: self
                    .number(
                        flags.stream_reopen_delay_ms,
                        bluetooth.and_then(|b| b.stream_reopen_delay_ms),
                        env_keys::BT_STREAM_REOPEN_DELAY_MS,
                    )
                    .unwrap_or(DEFAULT_STREAM_REOPEN_DELAY_MS),
                use_mock_backend: self
                    .boolean(
                        flags.use_mock_backend,
                        bluetooth.and_then(|b| b.use_mock_backend),
                        env_keys::BT_USE_MOCK_BACKEND,
                    )
                    .unwrap_or(false),
            },
            location: self.position(flags, file.location.as_ref(), bluetooth),
            ca: CaConfig {
                key_path: self
                    .text(
                        None,
                        file.ca.as_ref().and_then(|c| c.key_path.clone()),
                        env_keys::CA_ROOT_KEY_PATH,
                    )
                    .unwrap_or_else(|| DEFAULT_CA_KEY_PATH.to_string()),
                validity_days: self
                    .number(
                        None,
                        file.ca.as_ref().and_then(|c| c.validity_days),
                        env_keys::CA_VALIDITY_DAYS,
                    )
                    .unwrap_or(DEFAULT_CA_VALIDITY_DAYS),
                node_type: self
                    .text(
                        None,
                        file.ca.as_ref().and_then(|c| c.node_type.clone()),
                        env_keys::CA_NODE_TYPE,
                    )
                    .unwrap_or_else(|| DEFAULT_CA_NODE_TYPE.to_string()),
            },
        };

        // A coordinate pair is only useful if it parses, and a typo in a config
        // file belongs in the startup errors rather than in the log every time an
        // occurrence is stored.
        if let Err(e) = config.location.fixed_coordinates() {
            self.errors.push(e.to_string());
        }

        config
    }

    /// `[location]`, folding in the deprecated `[bluetooth].fixed_location`.
    fn position(
        &mut self,
        flags: &Flags,
        section: Option<&LocationSection>,
        bluetooth: Option<&BluetoothSection>,
    ) -> PositionConfig {
        PositionConfig {
            mode: self.position_mode(flags, section),
            fixed: self.fixed_location(flags, section, bluetooth),
            gps: self.receiver(flags, section.and_then(|l| l.gps.as_ref())),
        }
    }

    fn position_mode(&mut self, flags: &Flags, section: Option<&LocationSection>) -> PositionMode {
        let from_flag = self.enumerated(
            flags.location_mode.as_deref(),
            "--location-mode",
            parse_position_mode,
        );
        let from_file = section.and_then(|l| l.mode);

        let env_raw = self.env.get(env_keys::LOCATION_MODE);
        let from_env = self.enumerated(
            env_raw.as_deref(),
            env_keys::LOCATION_MODE,
            parse_position_mode,
        );

        from_flag.or(from_file).or(from_env).unwrap_or_default()
    }

    fn fixed_location(
        &mut self,
        flags: &Flags,
        section: Option<&LocationSection>,
        bluetooth: Option<&BluetoothSection>,
    ) -> Option<String> {
        let current = self.text(
            flags.fixed_location.clone(),
            section.and_then(|l| l.fixed.clone()),
            env_keys::LOCATION_FIXED,
        );
        if current.is_some() {
            return current;
        }

        // Deprecated spellings, consulted only while the current one is silent,
        // and reported when they are what actually took effect.
        if let Some(raw) = bluetooth.and_then(|b| b.fixed_location.clone()) {
            self.notes.push(
                "[bluetooth].fixed_location is deprecated; the fixed location now lives at \
                 [location].fixed"
                    .to_string(),
            );
            return Some(raw);
        }

        let legacy_env = self.env.get(env_keys::FIXED_LOCATION);
        if legacy_env.is_some() {
            self.notes.push(format!(
                "{} is deprecated; the fixed location now lives at [location].fixed or {}",
                env_keys::FIXED_LOCATION,
                env_keys::LOCATION_FIXED
            ));
        }
        legacy_env
    }

    /// `[location.gps]`.
    ///
    /// `None` means no receiver is configured anywhere, which is what stops
    /// `mode = "auto"` from opening a TCP connection to a port nobody listens on
    /// once per occurrence. Naming the receiver in *any* layer — a `[location.gps]`
    /// header, a flag, or one `BT_GPS_*` variable — is enough to bring it into
    /// being, with the remaining fields defaulted.
    fn receiver(&mut self, flags: &Flags, section: Option<&GpsSection>) -> Option<GpsSettings> {
        let env_names = [
            env_keys::GPS_BACKEND,
            env_keys::GPS_HOST,
            env_keys::GPS_PORT,
            env_keys::GPS_TIMEOUT_MS,
            env_keys::GPS_MAX_AGE_MS,
            env_keys::GPS_RETRY_BACKOFF_MS,
        ];
        let mentioned_in_env = env_names.iter().any(|name| self.env.get(name).is_some());

        if section.is_none()
            && flags.gps_backend.is_none()
            && flags.gps_host.is_none()
            && flags.gps_port.is_none()
            && flags.gps_timeout_ms.is_none()
            && !mentioned_in_env
        {
            return None;
        }

        let from_flag = self.enumerated(
            flags.gps_backend.as_deref(),
            "--gps-backend",
            parse_gps_backend,
        );
        let from_file = section.and_then(|g| g.backend);
        let env_raw = self.env.get(env_keys::GPS_BACKEND);
        let from_env =
            self.enumerated(env_raw.as_deref(), env_keys::GPS_BACKEND, parse_gps_backend);

        // GpsSettings owns the receiver defaults, so this file does not restate them.
        let defaults = GpsSettings::default();

        Some(GpsSettings {
            backend: from_flag.or(from_file).or(from_env).unwrap_or_default(),
            host: self
                .text(
                    flags.gps_host.clone(),
                    section.and_then(|g| g.host.clone()),
                    env_keys::GPS_HOST,
                )
                .unwrap_or_else(|| defaults.host.clone()),
            port: self
                .number(
                    flags.gps_port,
                    section.and_then(|g| g.port),
                    env_keys::GPS_PORT,
                )
                .unwrap_or(defaults.port),
            timeout_ms: self
                .number(
                    flags.gps_timeout_ms,
                    section.and_then(|g| g.timeout_ms),
                    env_keys::GPS_TIMEOUT_MS,
                )
                .unwrap_or(defaults.timeout_ms),
            max_age_ms: self
                .number::<u64>(
                    None,
                    section.and_then(|g| g.max_age_ms),
                    env_keys::GPS_MAX_AGE_MS,
                )
                .unwrap_or(defaults.max_age_ms),
            retry_backoff_ms: self
                .number::<u64>(
                    None,
                    section.and_then(|g| g.retry_backoff_ms),
                    env_keys::GPS_RETRY_BACKOFF_MS,
                )
                .unwrap_or(defaults.retry_backoff_ms),
        })
    }
}

/// `true`/`false`, `1`/`0`, `yes`/`no`, `on`/`off`.
///
/// Environment variables get set from shells, `.env` files and container
/// manifests, and each has its own idea of what true looks like. Anything else is
/// an error rather than a guess: reading `BT_USE_MOCK_BACKEND=ture` as `false` is
/// exactly the silent divergence that costs an afternoon.
fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_position_mode(raw: &str) -> Result<PositionMode, String> {
    match raw.trim().to_lowercase().as_str() {
        "auto" => Ok(PositionMode::Auto),
        "fixed" => Ok(PositionMode::Fixed),
        "gps" => Ok(PositionMode::Gps),
        "off" => Ok(PositionMode::Off),
        other => Err(format!(
            "unknown location mode '{other}' (expected auto, fixed, gps or off)"
        )),
    }
}

fn parse_gps_backend(raw: &str) -> Result<GpsBackend, String> {
    match raw.trim().to_lowercase().as_str() {
        "gpsd" => Ok(GpsBackend::Gpsd),
        "mock" => Ok(GpsBackend::Mock),
        other => Err(format!(
            "unknown gps backend '{other}' (expected gpsd or mock)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An env layer with exactly these variables.
    ///
    /// Deliberately insulated from the process environment: the ambient env of
    /// whoever runs `cargo test` must not decide whether these pass.
    fn env(pairs: &[(&str, &str)]) -> EnvLayer {
        EnvLayer::from_map(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        )
    }

    fn parse_file(toml: &str) -> ConfigFile {
        ConfigFile::parse(toml).expect("test fixture should parse")
    }

    fn resolved(
        flags: &Flags,
        contents: &str,
        env_vars: &[(&str, &str)],
    ) -> Result<Resolved, Vec<String>> {
        resolve(flags, &parse_file(contents), &env(env_vars))
    }

    const RATE_FILE: &str = "[bluetooth]\nrate_limit_ms = 7000\n";
    const OWNED_CELLS_FILE: &str = "[node]\nowns_cells = [\"862a1072fffffff\"]\n";

    #[test]
    fn nothing_stated_yields_the_built_in_defaults() {
        let config = resolved(&Flags::default(), "", &[]).unwrap().config;

        assert_eq!(config.log.level, DEFAULT_LOG_LEVEL);
        assert_eq!(config.bluetooth.rate_limit_ms, DEFAULT_RATE_LIMIT_MS);
        assert_eq!(config.bluetooth.scan_interval_ms, DEFAULT_SCAN_INTERVAL_MS);
        assert_eq!(
            config.bluetooth.stream_reopen_delay_ms,
            DEFAULT_STREAM_REOPEN_DELAY_MS
        );
        assert!(config.bluetooth.store_raw_payload);
        assert!(!config.bluetooth.use_mock_backend);
        assert_eq!(config.ca.validity_days, DEFAULT_CA_VALIDITY_DAYS);
        assert_eq!(config.ca.node_type, DEFAULT_CA_NODE_TYPE);
        assert_eq!(config.ca.key_path, DEFAULT_CA_KEY_PATH);
        assert!(config.node.id.is_none());
        assert!(config.node.owns_cells.is_empty());
    }

    #[test]
    fn the_environment_spells_a_cell_list_with_commas() {
        let config = resolved(
            &Flags::default(),
            "",
            &[("BT_OWNS_CELLS", " 862a1072fffffff , 860326237ffffff ,")],
        )
        .unwrap()
        .config;

        assert_eq!(
            config.node.owns_cells,
            vec!["862a1072fffffff", "860326237ffffff"]
        );
    }

    #[test]
    fn the_file_cell_list_beats_the_environment_one() {
        let config = resolved(
            &Flags::default(),
            OWNED_CELLS_FILE,
            &[("BT_OWNS_CELLS", "860326237ffffff")],
        )
        .unwrap()
        .config;

        assert_eq!(config.node.owns_cells, vec!["862a1072fffffff"]);
    }

    #[test]
    fn a_flag_beats_the_file_and_the_environment() {
        let flags = Flags {
            rate_limit_ms: Some(1000),
            ..Default::default()
        };
        let config = resolved(&flags, RATE_FILE, &[("BT_RATE_LIMIT_MS", "2000")])
            .unwrap()
            .config;

        assert_eq!(config.bluetooth.rate_limit_ms, 1000);
    }

    #[test]
    fn the_file_beats_the_environment() {
        let config = resolved(
            &Flags::default(),
            RATE_FILE,
            &[("BT_RATE_LIMIT_MS", "2000")],
        )
        .unwrap()
        .config;

        assert_eq!(config.bluetooth.rate_limit_ms, 7000);
    }

    #[test]
    fn the_environment_is_used_when_the_file_is_silent() {
        let config = resolved(&Flags::default(), "", &[("BT_RATE_LIMIT_MS", "2000")])
            .unwrap()
            .config;

        assert_eq!(config.bluetooth.rate_limit_ms, 2000);
    }

    #[test]
    fn a_file_value_equal_to_the_default_is_not_mistaken_for_a_default() {
        // The bug this design removes: `merge_with_file` compared against
        // sentinel defaults, so a file value was ignored whenever the resolved
        // value happened to look like the default.
        let config = resolved(
            &Flags::default(),
            "[log]\nlevel = \"info\"\n",
            &[("LOG_LEVEL", "debug")],
        )
        .unwrap()
        .config;

        assert_eq!(
            config.log.level, "info",
            "the file states info, which outranks the env layer"
        );
    }

    #[test]
    fn an_explicit_flag_that_looks_like_the_default_still_wins() {
        let flags = Flags {
            log_level: Some("info".to_string()),
            ..Default::default()
        };
        let config = resolved(&flags, "", &[("LOG_LEVEL", "debug")])
            .unwrap()
            .config;

        assert_eq!(config.log.level, "info");
    }

    #[test]
    fn database_settings_come_from_whichever_layer_states_them() {
        let config = resolved(
            &Flags {
                pg_password: Some("from-flag".to_string()),
                ..Default::default()
            },
            "[database]\nurl = \"postgres://file/db\"\nuser = \"from-file\"\n",
            &[("PGDATABASE", "from-env")],
        )
        .unwrap()
        .config;

        assert_eq!(config.database.password.as_deref(), Some("from-flag"));
        assert_eq!(config.database.user.as_deref(), Some("from-file"));
        assert_eq!(config.database.database.as_deref(), Some("from-env"));
        assert_eq!(config.database.url.as_deref(), Some("postgres://file/db"));
    }

    #[test]
    fn booleans_in_the_environment_are_read_loosely() {
        for raw in ["true", "1", "YES", "on"] {
            let config = resolved(&Flags::default(), "", &[("BT_USE_MOCK_BACKEND", raw)])
                .unwrap()
                .config;
            assert!(config.bluetooth.use_mock_backend, "{raw} should mean true");
        }

        let config = resolved(&Flags::default(), "", &[("BT_STORE_RAW_PAYLOAD", "no")])
            .unwrap()
            .config;
        assert!(!config.bluetooth.store_raw_payload);
    }

    #[test]
    fn a_present_flag_is_true_even_though_the_default_is_false() {
        let flags = Flags {
            use_mock_backend: Some(true),
            ..Default::default()
        };

        let config = resolved(&flags, "", &[]).unwrap().config;
        assert!(config.bluetooth.use_mock_backend);
    }

    #[test]
    fn malformed_environment_values_are_reported_against_their_variable() {
        let errors = resolved(
            &Flags::default(),
            "",
            &[("PGPORT", "postgres"), ("BT_USE_MOCK_BACKEND", "ture")],
        )
        .unwrap_err();

        assert_eq!(
            errors.len(),
            2,
            "both problems should be reported: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("PGPORT") && e.contains("postgres")),
            "the port error should name the value: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("BT_USE_MOCK_BACKEND")),
            "the boolean error should name the variable: {errors:?}"
        );
    }

    #[test]
    fn the_file_layer_cannot_produce_a_malformed_number() {
        // serde rejects `port = "not-a-number"` before the resolver sees it, so
        // the resolver only ever has to explain env mistakes.
        let error = ConfigFile::parse("[database]\nport = \"nope\"\n").unwrap_err();
        assert!(error.contains("port"), "got: {error}");
    }

    #[test]
    fn location_fixed_prefers_the_current_key_over_the_deprecated_one() {
        let config = resolved(
            &Flags::default(),
            "[location]\nfixed = \"1.5,2.5\"\n[bluetooth]\nfixed_location = \"9.9,9.9\"\n",
            &[],
        )
        .unwrap()
        .config;

        assert_eq!(config.location.fixed.as_deref(), Some("1.5,2.5"));
    }

    #[test]
    fn a_deprecated_fixed_location_still_works_and_says_so() {
        let resolved = resolved(
            &Flags::default(),
            "[bluetooth]\nfixed_location = \"40.6892,-74.0445\"\n",
            &[],
        )
        .unwrap();

        assert_eq!(
            resolved.config.location.fixed.as_deref(),
            Some("40.6892,-74.0445")
        );
        assert_eq!(resolved.notes.len(), 1, "got: {:?}", resolved.notes);
        assert!(resolved.notes[0].contains("[bluetooth].fixed_location"));
        assert!(resolved.notes[0].contains("[location].fixed"));
    }

    #[test]
    fn the_deprecated_environment_variable_also_notes() {
        let resolved = resolved(
            &Flags::default(),
            "",
            &[("BT_FIXED_LOCATION", "40.6892,-74.0445")],
        )
        .unwrap();

        assert_eq!(
            resolved.config.location.fixed.as_deref(),
            Some("40.6892,-74.0445")
        );
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("BT_FIXED_LOCATION")),
            "got: {:?}",
            resolved.notes
        );
    }

    #[test]
    fn a_deprecated_key_that_lost_is_not_reported() {
        let resolved = resolved(
            &Flags {
                fixed_location: Some("1.0,2.0".to_string()),
                ..Default::default()
            },
            "[bluetooth]\nfixed_location = \"9.9,9.9\"\n",
            &[],
        )
        .unwrap();

        assert_eq!(resolved.config.location.fixed.as_deref(), Some("1.0,2.0"));
        assert!(resolved.notes.is_empty(), "got: {:?}", resolved.notes);
    }

    #[test]
    fn a_receiver_is_only_brought_into_being_when_somewhere_it_is_named() {
        assert!(resolved(&Flags::default(), "", &[])
            .unwrap()
            .config
            .location
            .gps
            .is_none());

        let header_only = resolved(&Flags::default(), "[location.gps]\n", &[])
            .unwrap()
            .config
            .location
            .gps
            .expect("[location.gps] states a receiver exists");
        assert_eq!(header_only.port, GpsSettings::default().port);
        assert_eq!(header_only.backend, GpsBackend::Gpsd);

        let one_variable = resolved(&Flags::default(), "", &[("BT_GPS_HOST", "192.168.1.9")])
            .unwrap()
            .config
            .location
            .gps
            .expect("naming one setting states a receiver exists");
        assert_eq!(one_variable.host, "192.168.1.9");
    }

    #[test]
    fn receiver_settings_follow_the_same_precedence() {
        let flags = Flags {
            gps_port: Some(2950),
            gps_backend: Some("mock".to_string()),
            ..Default::default()
        };
        let config = resolved(
            &flags,
            "[location.gps]\nport = 2948\nhost = \"file-host\"\nmax_age_ms = 5000\n",
            &[("BT_GPS_PORT", "2949"), ("BT_GPS_TIMEOUT_MS", "750")],
        )
        .unwrap()
        .config;

        let gps = config.location.gps.unwrap();
        assert_eq!(gps.port, 2950, "flag wins");
        assert_eq!(gps.backend, GpsBackend::Mock, "flag wins");
        assert_eq!(gps.host, "file-host", "file wins over the default");
        assert_eq!(gps.max_age_ms, 5000);
        assert_eq!(gps.timeout_ms, 750, "env fills what the file omitted");
        assert_eq!(
            gps.retry_backoff_ms,
            GpsSettings::default().retry_backoff_ms
        );
    }

    #[test]
    fn an_unknown_mode_or_backend_names_the_layer_that_supplied_it() {
        let flags = Flags {
            location_mode: Some("wherever".to_string()),
            ..Default::default()
        };
        let errors = resolved(&flags, "", &[]).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("--location-mode"),
            "should blame the flag: {errors:?}"
        );

        let errors = resolved(&Flags::default(), "", &[("BT_GPS_BACKEND", "glonass")]).unwrap_err();
        assert!(
            errors[0].contains("BT_GPS_BACKEND"),
            "should blame the variable: {errors:?}"
        );
    }

    #[test]
    fn the_location_mode_follows_precedence() {
        let flags = Flags {
            location_mode: Some("off".to_string()),
            ..Default::default()
        };
        let config = resolved(
            &flags,
            "[location]\nmode = \"gps\"\n",
            &[("BT_LOCATION_MODE", "fixed")],
        )
        .unwrap()
        .config;

        assert_eq!(config.location.mode, PositionMode::Off);

        let config = resolved(
            &Flags::default(),
            "[location]\nmode = \"gps\"\n",
            &[("BT_LOCATION_MODE", "fixed")],
        )
        .unwrap()
        .config;
        assert_eq!(config.location.mode, PositionMode::Gps);

        let config = resolved(&Flags::default(), "", &[("BT_LOCATION_MODE", "off")])
            .unwrap()
            .config;
        assert_eq!(config.location.mode, PositionMode::Off);
    }

    #[test]
    fn a_malformed_fixed_point_is_a_startup_error() {
        let errors =
            resolved(&Flags::default(), "[location]\nfixed = \"here\"\n", &[]).unwrap_err();

        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("here"),
            "should quote the value: {errors:?}"
        );
    }

    #[test]
    fn the_ca_section_fills_what_a_subcommand_would_otherwise_default() {
        let config = resolved(
            &Flags::default(),
            "[ca]\nkey_path = \"/etc/btmon/root.hex\"\nvalidity_days = 30\nnode_type = \"aggregator\"\n",
            &[],
        )
        .unwrap()
        .config;

        assert_eq!(config.ca.key_path, "/etc/btmon/root.hex");
        assert_eq!(config.ca.validity_days, 30);
        assert_eq!(config.ca.node_type, "aggregator");
    }

    #[test]
    fn an_unnamed_config_file_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[bluetooth]\nrate_limit_ms = 1\n",
        )
        .unwrap();

        let config = load_from(&Flags::default(), dir.path()).unwrap().config;

        assert_eq!(
            config.bluetooth.rate_limit_ms, DEFAULT_RATE_LIMIT_MS,
            "an implicit config.toml must not change behaviour"
        );
        assert!(config.config_file.is_none());
    }

    #[test]
    fn a_named_config_file_is_read_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.toml");
        std::fs::write(&path, "[bluetooth]\nrate_limit_ms = 1\n").unwrap();

        let flags = Flags {
            config_file: Some(path.display().to_string()),
            ..Default::default()
        };
        let config = load_from(&flags, dir.path()).unwrap().config;

        assert_eq!(config.bluetooth.rate_limit_ms, 1);
        assert_eq!(config.config_file.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn a_named_config_file_that_is_missing_is_an_error_not_a_shrug() {
        let dir = tempfile::tempdir().unwrap();
        let flags = Flags {
            config_file: Some(dir.path().join("nope.toml").display().to_string()),
            ..Default::default()
        };

        let errors = load_from(&flags, dir.path()).unwrap_err();
        assert!(errors[0].contains("nope.toml"), "got: {errors:?}");
    }

    #[test]
    fn load_picks_up_the_env_file_from_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "BT_RATE_LIMIT_MS=4321\n").unwrap();

        let config = load_from(&Flags::default(), dir.path()).unwrap().config;

        assert_eq!(config.bluetooth.rate_limit_ms, 4321);
    }

    #[test]
    fn the_flag_layer_can_still_override_the_env_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "BT_RATE_LIMIT_MS=4321\n").unwrap();

        let flags = Flags {
            rate_limit_ms: Some(9),
            ..Default::default()
        };
        let config = load_from(&flags, dir.path()).unwrap().config;

        assert_eq!(config.bluetooth.rate_limit_ms, 9);
    }

    #[test]
    fn the_default_data_directory_is_under_home() {
        // Only asserts the shape: the value depends on the runner's HOME.
        if std::env::var("HOME").is_err() {
            return;
        }

        let path = super::super::default_data_dir().unwrap();
        assert!(path.ends_with(".btmon/data"), "got: {}", path.display());
    }
}
