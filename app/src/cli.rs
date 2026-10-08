//! CLI subcommands for the Bluetooth monitoring application.
//!
//! A subcommand flag that duplicates a configuration setting carries no clap
//! default and no `env` attribute: it is an `Option`, and its absence hands the
//! decision to [`AppConfig`], which has already applied the precedence rule. A
//! `default_value` here would outrank the config file, and an `env` attribute
//! would bypass it entirely.

use clap::{Parser, Subcommand};
use log::debug;
use repo::models::NodeType;
use repo::{CellIndex, NodeRepository, Resolution, SignalType};
use sqlx::postgres::PgPoolOptions;
use std::path::PathBuf;

use crate::config::{self, AppConfig, Flags};
use crate::position::BestEffortPositionSource;

// Import RslManager trait for its methods
use ca::RslManager;

type Pool = sqlx::Pool<sqlx::Postgres>;

/// CLI subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Start Bluetooth monitoring (default)
    Monitor {
        /// Node id this node has to turn out to be (64 hex characters)
        ///
        /// A node id is SHA-256 of the signing key in the data directory, so this
        /// asserts an identity rather than setting one: startup stops when the key
        /// disagrees. Omit it to accept the key the node already has.
        #[arg(long)]
        node_id: Option<String>,
    },

    /// Query occurrences from the database
    Query {
        /// Query by time range (e.g., "1h" for last hour, "30m" for last 30 minutes)
        #[arg(long)]
        last: Option<String>,

        /// Query by H3 geo cell (macro resolution)
        ///
        /// Hex or decimal, as `psql` prints it. Must be a res 6 cell: one at any
        /// other resolution matches no row, and clap says so rather than letting
        /// the answer look like an empty area.
        #[arg(long, value_name = "CELL", value_parser = parse_macro_cell)]
        geo_cell: Option<CellIndex>,

        /// Query by signal type
        #[arg(long, default_value = "bluetooth")]
        signal_type: String,

        /// Maximum number of results
        #[arg(long, default_value = "100")]
        limit: i64,

        /// Output format (text, json)
        #[arg(long, default_value = "text")]
        format: String,
    },

    /// Show node and database statistics
    Stats {
        /// Database URL
        #[arg(short, long)]
        database_url: Option<String>,
    },

    /// Replay stored advertisements through the device-identity resolver
    ///
    /// Runs the identity resolver offline over rows already in `occurrences` and prints
    /// every decision it made — a new identity, a merge, or a refusal — with the evidence
    /// behind it. Nothing is written without `--write`, and what it does write is the
    /// derived tables: `occurrences` itself is never touched, because those rows are
    /// signed assertions by a node and a derived table has no business editing them.
    ///
    /// The point is to read the merges before anything depends on them. Each line carries
    /// the score *and* the coverage, because a 65-point merge resting on a name and a
    /// signal level is a different claim from a 65-point merge resting on a manufacturer
    /// id, an advertisement layout, and a payload.
    IdentityReplay {
        /// Time range to replay (e.g. "24h", "7d"); the default is everything
        #[arg(long)]
        last: Option<String>,

        /// Replay only this node's occurrences (64 hex characters)
        ///
        /// Narrowing to one node is not only cheaper: an identity spanning two nodes is a
        /// claim about the network's coverage as well as about a device, and the first pass
        /// over a database should not be making one.
        #[arg(long)]
        node: Option<String>,

        /// Maximum occurrences to read
        #[arg(long, default_value = "10000")]
        limit: i64,

        /// Fraction of the identity model a merge needs to be accepted (default: 0.35)
        #[arg(long)]
        min_evidence_ratio: Option<f64>,

        /// Points at which two observations are taken to be one device (default: 40)
        #[arg(long)]
        merge_threshold: Option<f64>,

        /// Emit the report as JSON instead of text
        #[arg(long)]
        json: bool,

        /// Write `device_identities`, `device_address_links`, `co_occurrence_events` and
        /// `association_edges`
        #[arg(long)]
        write: bool,

        /// Database URL
        #[arg(short, long)]
        database_url: Option<String>,
    },

    /// CA (Certificate Authority) management commands
    #[command(subcommand)]
    Ca(CaCommands),
}

/// CA (Certificate Authority) subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum CaCommands {
    /// Initialize CA root key (generate and save)
    CaInit {
        /// Path to store the CA root key (default: `[ca].key_path`)
        #[arg(long)]
        key_path: Option<String>,

        /// Replace the key if one already exists
        #[arg(long)]
        force: bool,
    },

    /// Enroll a node (issue credential and write its `nodes` registry row)
    ///
    /// Occurrences reference `nodes(node_id)`, so a node cannot store anything
    /// until it is enrolled.
    CaEnroll {
        /// Node's Ed25519 signing key, hex-encoded (32 bytes)
        ///
        /// Kept for scripts. A node writes its key as `node_identity.pub.pem`, and
        /// `--public-key-file` reads that file instead of asking an operator to copy
        /// 64 characters out of it by hand.
        #[arg(long, conflicts_with = "public_key_file")]
        public_key: Option<String>,

        /// Node's signing key as an SPKI PEM file (`node_identity.pub.pem`)
        #[arg(long)]
        public_key_file: Option<String>,

        /// Node's display name (optional, for logging)
        #[arg(long)]
        display_name: Option<String>,

        /// Credential validity in days (default: `[ca].validity_days`)
        #[arg(long)]
        validity_days: Option<u64>,

        /// Path to CA root key (default: `[ca].key_path`)
        #[arg(long)]
        key_path: Option<String>,

        /// Output credential to file (instead of stdout)
        #[arg(long)]
        output: Option<String>,

        /// Node type to record (default: `[ca].node_type`)
        #[arg(long)]
        node_type: Option<String>,

        /// Fixed latitude; requires --lon
        #[arg(long)]
        lat: Option<f64>,

        /// Fixed longitude; requires --lat
        #[arg(long)]
        lon: Option<f64>,

        /// Res 6 H3 cell this node is authoritative for (hex or decimal)
        ///
        /// Repeatable. Given at all, it replaces both the configured claim and
        /// the cell derived from the node's location; a node that says which cells
        /// it owns is not also handed the one its antenna happens to sit in.
        #[arg(long, value_name = "CELL")]
        owns_cell: Vec<String>,

        /// Database URL
        #[arg(short, long)]
        database_url: Option<String>,
    },

    /// Verify a node's credential
    CaVerify {
        /// Node ID (hex-encoded, 32 bytes)
        #[arg(long)]
        node_id: String,

        /// Path to CA root key (default: `[ca].key_path`)
        #[arg(long)]
        key_path: Option<String>,

        /// Path to credential file (if not in database)
        #[arg(long)]
        credential_file: Option<String>,
    },

    /// Revoke a node's credentials
    ///
    /// Records the revocation in `node_revocations` and marks the registry row
    /// revoked. It publishes nothing: other nodes see the revocation when the CA
    /// publishes a signed list with `ca ca-generate-rsl`, which is also when the
    /// ledger row becomes attributable to that CA. No root key is read here —
    /// holding it is what publication proves, and this command stores a revocation
    /// that is not yet signed by anyone.
    CaRevoke {
        /// Node ID to revoke (hex-encoded, 32 bytes)
        #[arg(long)]
        node_id: String,

        /// Why: unspecified, key-compromise, ca-compromise, ceased-operation,
        /// policy-violation, superseded, hold
        #[arg(long, default_value = "unspecified")]
        reason: String,

        /// Database URL holding the nodes registry
        #[arg(long)]
        database_url: Option<String>,
    },

    /// Show CA information (public key, status)
    CaInfo {
        /// Path to CA root key (default: `[ca].key_path`)
        #[arg(long)]
        key_path: Option<String>,
    },

    /// Generate a Revocation Status List (RSL)
    CaGenerateRsl {
        /// Path to CA root key (default: `[ca].key_path`)
        #[arg(long)]
        key_path: Option<String>,

        /// Database URL for reading revocations
        #[arg(long)]
        database_url: Option<String>,

        /// Output RSL to file
        #[arg(long)]
        output: Option<String>,

        /// RSL validity in days (default: 1)
        #[arg(long, default_value = "1")]
        validity_days: u64,
    },

    /// Publish this CA's trust anchor: the public key other nodes verify with
    ///
    /// Writes a SubjectPublicKeyInfo PEM document (`-----BEGIN PUBLIC KEY-----`),
    /// which is the half meant to leave this machine. Distribute it to every node
    /// that has to check this CA's revocation lists and credentials; it verifies and
    /// never signs, so copying it costs nothing in secrecy.
    CaExportAnchor {
        /// Path to CA root key (default: `[ca].key_path`)
        #[arg(long)]
        key_path: Option<String>,

        /// Where to write the anchor (default: the root key's path with a
        /// `.pub.pem` extension)
        #[arg(long)]
        output: Option<String>,
    },

    /// Convert a pre-PKCS#8 hex root key file into a PKCS#8 PEM key file
    ///
    /// Root keys were hex until PKCS#8 arrived. The conversion preserves the CA's
    /// identity — the id is derived from the public half — so credentials and lists
    /// this CA already published keep verifying afterwards.
    CaMigrateKey {
        /// The legacy key file (64 hex characters, no PEM envelope)
        #[arg(long)]
        from: String,

        /// Where to write the PEM (default: `--from` with a `.pem` extension)
        #[arg(long)]
        output: Option<String>,
    },
}

/// Resolved arguments for `ca enroll`.
///
/// Bundled rather than passed positionally because most fields are the same
/// type, and swapping `key_path` with `database_url` would fail in a confusing
/// way — as would swapping `public_key` with `public_key_file`, both of which are
/// `Option<&str>` and one of which is a key and the other a filename.
struct EnrollRequest<'a> {
    public_key: Option<&'a str>,
    public_key_file: Option<&'a str>,
    display_name: Option<&'a str>,
    validity_days: Option<u64>,
    key_path: Option<&'a str>,
    output: Option<&'a str>,
    node_type: Option<&'a str>,
    lat: Option<f64>,
    lon: Option<f64>,
    owns_cells: &'a [String],
    database_url: Option<&'a str>,
}

/// A parsed invocation: the subcommand to run and the configuration to run it with.
pub struct Cli {
    pub command: Commands,
    pub config: AppConfig,
    /// How the configuration came to be what it is — deprecated settings, mostly.
    ///
    /// Carried rather than logged here because the logger is not initialized
    /// until the resolved log level is known.
    pub config_notes: Vec<String>,
}

/// Everything accepted before the subcommand, as typed.
///
/// No clap defaults and no `env` attributes: the config file and the environment
/// are separate layers with their own ordering, and a defaulted flag would be
/// indistinguishable from one the operator typed.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// TOML file to read configuration from (also `$CONFIG_FILE`)
    #[arg(long, env = "CONFIG_FILE")]
    config_file: Option<String>,

    /// PostgreSQL connection string
    #[arg(short, long)]
    database_url: Option<String>,

    /// PostgreSQL host
    #[arg(long)]
    pg_host: Option<String>,

    /// PostgreSQL port
    #[arg(long)]
    pg_port: Option<u16>,

    /// PostgreSQL database name
    #[arg(long)]
    pg_database: Option<String>,

    /// PostgreSQL user
    #[arg(long)]
    pg_user: Option<String>,

    /// PostgreSQL password
    #[arg(long)]
    pg_password: Option<String>,

    /// Log level (debug, info, warn, error)
    #[arg(long)]
    log_level: Option<String>,

    /// Node id this node has to turn out to be (64 hex characters)
    ///
    /// Checked against the identity loaded from the data directory; it cannot
    /// choose one. See `monitor --node-id`.
    #[arg(long)]
    node_id: Option<String>,

    /// Bluetooth adapter to monitor, by id, name, or MAC address
    ///
    /// Matched case-insensitively against the whole adapter description or its
    /// first word — `hci1` in `hci1 (usb:1d6b:0003)`. A selector that matches
    /// nothing, or matches more than one adapter, stops startup instead of
    /// picking one. Omit it to take the first adapter the system reports.
    #[arg(long)]
    adapter_id: Option<String>,

    /// Data directory for node identity
    #[arg(long)]
    data_dir: Option<String>,

    /// How often to re-arm the scan, in milliseconds
    ///
    /// Scanning is continuous: this is not a duty cycle. Every interval the
    /// monitor issues a fresh stop/start, so a controller-side scan that went
    /// stale cannot leave a running node silently deaf. It is also how often the
    /// simulated radio advertises.
    #[arg(long)]
    scan_interval_ms: Option<u64>,

    /// Store the radio's advertisement bytes in `signal_payload`
    ///
    /// Adds `ble.raw_payload_hex` to occurrences that have advertisement bytes,
    /// inside the signed payload. A backend that hands over only decoded
    /// properties stores nothing extra and says so once.
    ///
    /// Bare means yes; `--store-raw-payload=false` is how to say no from the
    /// command line. Omitting it leaves the decision to a lower layer. The value
    /// has to be attached so a bare flag can never swallow the word after it.
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        require_equals = true,
        value_name = "BOOL"
    )]
    store_raw_payload: Option<bool>,

    /// Rate limit threshold in milliseconds
    #[arg(long)]
    rate_limit_ms: Option<u64>,

    /// Delay (ms) before reopening a closed device event stream
    #[arg(long)]
    stream_reopen_delay_ms: Option<u64>,

    /// Use mock Bluetooth backend (for testing/development)
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        require_equals = true,
        value_name = "BOOL"
    )]
    use_mock_backend: Option<bool>,

    /// Fixed location as "lat,lon" (same as `[location].fixed`)
    #[arg(long)]
    fixed_location: Option<String>,

    /// How to acquire a position: auto, fixed, gps or off
    #[arg(long)]
    location_mode: Option<String>,

    /// Receiver to use: gpsd or mock
    #[arg(long)]
    gps_backend: Option<String>,

    /// gpsd host
    #[arg(long)]
    gps_host: Option<String>,

    /// gpsd port
    #[arg(long)]
    gps_port: Option<u16>,

    /// How long to wait for a receiver report, in milliseconds
    #[arg(long)]
    gps_timeout_ms: Option<u64>,

    #[command(subcommand)]
    command: Option<Commands>,
}

impl Args {
    /// The flag layer, holding only what was actually typed.
    fn flags(&self) -> Flags {
        Flags {
            config_file: self.config_file.clone(),
            database_url: self.database_url.clone(),
            pg_host: self.pg_host.clone(),
            pg_port: self.pg_port,
            pg_database: self.pg_database.clone(),
            pg_user: self.pg_user.clone(),
            pg_password: self.pg_password.clone(),
            log_level: self.log_level.clone(),
            node_id: self.stated_node_id(),
            adapter_id: self.adapter_id.clone(),
            data_dir: self.data_dir.clone(),
            scan_interval_ms: self.scan_interval_ms,
            store_raw_payload: self.store_raw_payload,
            rate_limit_ms: self.rate_limit_ms,
            stream_reopen_delay_ms: self.stream_reopen_delay_ms,
            use_mock_backend: self.use_mock_backend,
            fixed_location: self.fixed_location.clone(),
            location_mode: self.location_mode.clone(),
            gps_backend: self.gps_backend.clone(),
            gps_host: self.gps_host.clone(),
            gps_port: self.gps_port,
            gps_timeout_ms: self.gps_timeout_ms,
        }
    }

    /// The node id as stated for this run, from either position clap allows.
    ///
    /// `monitor --node-id` is the narrower spelling of the global `--node-id`, so
    /// it wins. It has to land in the flag layer and not only in the [`Commands`]
    /// value: the configuration is resolved from the flags, and a value that
    /// reaches one and not the other is read off the command line and then
    /// dropped — which is exactly what this setting used to do.
    fn stated_node_id(&self) -> Option<String> {
        let from_monitor = match &self.command {
            Some(Commands::Monitor { node_id }) => node_id.clone(),
            _ => None,
        };

        from_monitor.or_else(|| self.node_id.clone())
    }

    /// The subcommand to run, defaulting to the monitor.
    ///
    /// `--node-id` is accepted both before and after `monitor`; the narrower
    /// position is the one the operator meant for this run.
    fn command(&self) -> Commands {
        match self.command.clone() {
            Some(Commands::Monitor { .. }) => Commands::Monitor {
                node_id: self.stated_node_id(),
            },
            Some(other) => other,
            None => Commands::Monitor {
                node_id: self.stated_node_id(),
            },
        }
    }
}

impl Cli {
    /// Parse argv, read the layers it points at, and collapse them into one
    /// configuration.
    ///
    /// Problems come back as a list for the caller to print: the logger does not
    /// exist yet, and one run should report every mistake in the file.
    pub fn from_args() -> Result<Self, Vec<String>> {
        let args = Args::parse();
        let resolved = config::load(&args.flags())?;

        Ok(Self {
            command: args.command(),
            config: resolved.config,
            config_notes: resolved.notes,
        })
    }

    /// Run the selected command
    pub async fn run(self) -> Result<(), String> {
        self.run_command().await
    }

    /// The connection string for a subcommand: its own flag, then the resolved
    /// configuration.
    fn database_for(&self, flag: Option<&str>) -> Result<String, String> {
        flag.map(str::to_string)
            .or_else(|| self.config.database_url_opt())
            .ok_or_else(|| {
                "no database configured: pass --database-url, or set DATABASE_URL, or give \
                 [database].url in the config file"
                    .to_string()
            })
    }

    /// The CA root key path for a subcommand, defaulting to `[ca].key_path`.
    fn ca_key_path(&self, flag: Option<&str>) -> PathBuf {
        PathBuf::from(flag.unwrap_or(self.config.ca.key_path.as_str()))
    }

    /// The location to register for an enrolled node.
    async fn enroll_location(
        &self,
        lat: Option<f64>,
        lon: Option<f64>,
    ) -> Result<Option<(f64, f64)>, String> {
        // Only ask the receiver when the operator has not already answered.
        let acquired = match (lat, lon) {
            (None, None) => self.acquired_location().await,
            _ => None,
        };

        resolve_enroll_location(lat, lon, acquired)
    }

    /// One position from the configured source.
    ///
    /// Enrollment is a one-off, so it builds its own source rather than sharing
    /// the monitor's cache. A configuration that cannot produce a position
    /// (`mode = "off"`, no source at all) is not an error here: it registers the
    /// node with no location.
    async fn acquired_location(&self) -> Option<(f64, f64)> {
        let source = match self.config.position_source() {
            Ok(source) => source,
            Err(e) => {
                debug!("no position to register for this node: {e}");
                return None;
            }
        };

        BestEffortPositionSource::new(source)
            .locate()
            .await
            .map(|position| (position.latitude, position.longitude))
    }

    /// Run the selected subcommand against the CA or the database.
    ///
    /// The monitor is a placeholder here: `main` starts it through
    /// [`crate::app::App`], which owns the node identity and event loop a
    /// monitoring session needs.
    pub async fn run_command(&self) -> Result<(), String> {
        match &self.command {
            Commands::Monitor { node_id } => {
                // This would call the FullNode monitor
                // For now, just print a message
                println!("Monitor command: node_id={:?}", node_id);
                Ok(())
            }

            Commands::Query {
                ref last,
                geo_cell,
                ref signal_type,
                limit,
                ref format,
            } => {
                let url = self.database_for(None)?;
                self.run_query(
                    &url,
                    last.clone(),
                    *geo_cell,
                    signal_type.clone(),
                    *limit,
                    format,
                )
                .await
            }

            Commands::Stats { ref database_url } => {
                let url = self.database_for(database_url.as_deref())?;
                self.run_stats(&url).await
            }

            Commands::IdentityReplay {
                ref last,
                ref node,
                limit,
                min_evidence_ratio,
                merge_threshold,
                json,
                write,
                ref database_url,
            } => {
                let url = self.database_for(database_url.as_deref())?;
                self.run_identity_replay(
                    &url,
                    last.as_deref(),
                    node.as_deref(),
                    *limit,
                    *min_evidence_ratio,
                    *merge_threshold,
                    *json,
                    *write,
                )
                .await
            }

            Commands::Ca(ref ca_cmd) => self.run_ca_command(ca_cmd.clone()).await,
        }
    }

    /// Run query command
    async fn run_query(
        &self,
        url: &str,
        last: Option<String>,
        geo_cell: Option<CellIndex>,
        signal_type: String,
        limit: i64,
        format: &str,
    ) -> Result<(), String> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        // Parse signal type
        let signal_type = match signal_type.to_lowercase().as_str() {
            "bluetooth" | "ble" => SignalType::Bluetooth,
            "wifi" => SignalType::Wifi,
            "nfc" => SignalType::Nfc,
            "zigbee" => SignalType::Zigbee,
            _ => return Err(format!("Unknown signal type: {}", signal_type)),
        };

        // Build query based on parameters
        let occurrences = if let Some(ref last_str) = last {
            // Parse time duration (e.g., "1h", "30m", "7d")
            let duration = parse_duration(last_str)
                .map_err(|e| format!("Invalid duration '{}': {}", last_str, e))?;
            let since = chrono::Utc::now() - duration;

            repo::OccurrenceRepository::find_recent(&pool, signal_type, since, limit)
                .await
                .map_err(|e| format!("Query failed: {}", e))?
        } else if let Some(cell) = geo_cell {
            repo::OccurrenceRepository::find_by_geo_cell(&pool, cell, limit)
                .await
                .map_err(|e| format!("Query failed: {}", e))?
        } else {
            // Default: query by signal type
            repo::OccurrenceRepository::find_by_signal_type(&pool, signal_type, limit)
                .await
                .map_err(|e| format!("Query failed: {}", e))?
        };

        // Output results
        match format.to_lowercase().as_str() {
            "json" => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&occurrences)
                        .map_err(|e| format!("Failed to serialize: {}", e))?
                );
            }
            _ => {
                println!("Found {} occurrences:", occurrences.len());
                for occ in &occurrences {
                    println!(
                        "  {} - {:?} - RSSI: {}dBm - {}",
                        occ.occurrence_id, occ.signal_type, occ.rssi, occ.observed_at
                    );
                }
            }
        }

        Ok(())
    }

    /// Run stats command
    async fn run_stats(&self, url: &str) -> Result<(), String> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        // Every read below goes through `repo`: the counts and the rows are the
        // same queries the node uses, and decoding them here would mean the CLI
        // knowing things about the wire types that only `repo` is supposed to.
        let total_count = repo::OccurrenceRepository::count_all(&pool)
            .await
            .map_err(|e| format!("Failed to count occurrences: {}", e))?;

        let signal_counts = repo::OccurrenceRepository::count_by_signal_type(&pool)
            .await
            .map_err(|e| format!("Failed to count by signal type: {}", e))?;

        let node_count = repo::NodeRepository::count(&pool)
            .await
            .map_err(|e| format!("Failed to count nodes: {}", e))?;

        // Print stats
        println!("=== Database Statistics ===");
        println!("Total occurrences: {}", total_count);
        println!("Total nodes: {}", node_count);
        println!("\nOccurrences by signal type:");
        for (signal_type, count) in &signal_counts {
            println!("  {:?}: {}", signal_type, count);
        }

        Ok(())
    }

    /// Replay the stored record through the device-identity resolver.
    ///
    /// Read, adapt, resolve, report — and write only when asked. The adapter counts the
    /// rows it refused to adapt and says so out loud: a pass over 400 rows that adapted 5
    /// is a finding about what the capture path stores, not about the devices, and a
    /// report that quietly dropped 395 rows would read like a quiet radio.
    #[allow(clippy::too_many_arguments)]
    async fn run_identity_replay(
        &self,
        url: &str,
        last: Option<&str>,
        node: Option<&str>,
        limit: i64,
        min_evidence_ratio: Option<f64>,
        merge_threshold: Option<f64>,
        json: bool,
        write: bool,
    ) -> Result<(), String> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        // The lower bound when the operator did not state one. Year 1 rather than the
        // epoch: a `timestamptz` column can hold rows from before 1970, and a replay that
        // silently excluded them would report a smaller world than the database has.
        let from = match last {
            Some(spec) => {
                let duration = parse_duration(spec)
                    .map_err(|e| format!("Invalid duration '{}': {}", spec, e))?;
                chrono::Utc::now() - duration
            }
            None => chrono::DateTime::from_timestamp(-62_135_596_800, 0)
                .expect("year 1 is inside chrono's range"),
        };
        let node_id = node.map(decode_node_id).transpose()?;

        let occurrences = repo::OccurrenceRepository::find_in_window(
            &pool,
            SignalType::Bluetooth,
            from,
            chrono::Utc::now(),
            node_id.as_deref(),
            limit,
        )
        .await
        .map_err(|e| format!("Replay scan failed: {}", e))?;

        // Adaptation is where a row becomes something the identity model can consider, and
        // where it refuses. Both numbers belong in the report.
        let mut feeds = Vec::with_capacity(occurrences.len());
        let mut skipped = 0_usize;
        for occurrence in &occurrences {
            match crate::identity::observation_from_occurrence(occurrence) {
                Some(feed) => feeds.push(feed),
                None => skipped += 1,
            }
        }

        let mut config = bt_iden::ResolverConfig::new();
        if let Some(ratio) = min_evidence_ratio {
            config = config.with_min_evidence_ratio(ratio);
        }
        if let Some(threshold) = merge_threshold {
            config = config.with_merge_threshold(threshold);
        }
        let options = crate::identity::ReplayOptions {
            config,
            ..crate::identity::ReplayOptions::default()
        };

        let report = crate::identity::replay(&feeds, &options);

        // A `--json` run exists to be piped, so stdout carries the document and nothing
        // else; the notes that a human wants are still worth printing, one line at a time,
        // to stderr, where a terminal shows them and a pipeline ignores them.
        let note = |message: String| {
            if json {
                eprintln!("{message}");
            } else {
                println!("{message}");
            }
        };

        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .map_err(|e| format!("Failed to serialize: {}", e))?
            );
        } else {
            print!("{}", report.render_text());
        }

        if skipped > 0 {
            note(format!(
                "{skipped} of {} rows carried no bluetooth advertisement and were skipped",
                occurrences.len()
            ));
        }

        if write {
            let outcome =
                crate::identity::write_report(&pool, &report, &crate::identity::resolver_version())
                    .await
                    .map_err(|e| format!("Failed to write the derived identity tables: {}", e))?;
            note(format!("wrote {}", outcome.summary()));
        } else {
            note(
                "report only: nothing was written. Pass --write to persist the derived \
                 identity tables."
                    .to_string(),
            );
        }

        Ok(())
    }

    /// Run CA command
    async fn run_ca_command(&self, cmd: CaCommands) -> Result<(), String> {
        match cmd {
            CaCommands::CaInit { key_path, force } => self.ca_init(key_path, force).await,
            CaCommands::CaEnroll {
                public_key,
                public_key_file,
                display_name,
                validity_days,
                key_path,
                output,
                node_type,
                lat,
                lon,
                owns_cell,
                database_url,
            } => {
                self.ca_enroll(EnrollRequest {
                    public_key: public_key.as_deref(),
                    public_key_file: public_key_file.as_deref(),
                    display_name: display_name.as_deref(),
                    validity_days,
                    key_path: key_path.as_deref(),
                    output: output.as_deref(),
                    node_type: node_type.as_deref(),
                    lat,
                    lon,
                    owns_cells: &owns_cell,
                    database_url: database_url.as_deref(),
                })
                .await
            }
            CaCommands::CaVerify {
                node_id,
                key_path,
                credential_file,
            } => self.ca_verify(node_id, key_path, credential_file).await,
            CaCommands::CaRevoke {
                node_id,
                reason,
                database_url,
            } => self.ca_revoke(node_id, reason, database_url).await,
            CaCommands::CaInfo { key_path } => self.ca_info(key_path).await,
            CaCommands::CaGenerateRsl {
                key_path,
                database_url,
                output,
                validity_days,
            } => {
                self.ca_generate_rsl(key_path, database_url, output, validity_days)
                    .await
            }
            CaCommands::CaExportAnchor { key_path, output } => {
                self.ca_export_anchor(key_path, output).await
            }
            CaCommands::CaMigrateKey { from, output } => self.ca_migrate_key(from, output).await,
        }
    }

    /// Initialize CA root key
    async fn ca_init(&self, key_path: Option<String>, force: bool) -> Result<(), String> {
        use std::fs;

        let key_path = self.ca_key_path(key_path.as_deref());

        // Check if key already exists
        if key_path.exists() && !force {
            println!("CA root key already exists at {}", key_path.display());
            println!("Use --force to overwrite (this will generate a NEW key!)");
            return Err("Key already exists. Use --force to overwrite.".to_string());
        }

        // Create parent directory if needed
        if let Some(parent) = key_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
        }

        // Generate new CA root
        let ca = ca::CaRoot::generate();

        // Save to file
        ca.save_to_file(&key_path, Some(0o600))
            .map_err(|e| format!("Failed to save CA root key: {}", e))?;

        println!("CA root key generated and saved to {}", key_path.display());
        println!("CA public key (hex): {}", hex::encode(ca.public_key()));
        println!("\nIMPORTANT: Back up this key securely!");
        println!("Any node's credential can only be verified with this CA public key.");

        Ok(())
    }

    /// The node's signing key, from whichever of the two flags named it.
    ///
    /// The file form matches how the key actually exists: a node writes
    /// `node_identity.pub.pem` and hands that over. Hex stays accepted because
    /// enrollment scripts already pass it — what is not acceptable is a tool that
    /// speaks *only* hex, which is how a key ends up retyped through a shell history.
    fn enroll_signing_key(
        public_key_hex: Option<&str>,
        public_key_file: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let key = match (public_key_hex, public_key_file) {
            (Some(hex_form), None) => {
                hex::decode(hex_form).map_err(|e| format!("Invalid --public-key hex: {e}"))?
            }
            (None, Some(path)) => {
                let contents = std::fs::read_to_string(path)
                    .map_err(|e| format!("Failed to read --public-key-file {path}: {e}"))?;
                ca::pemkeys::read_public_key_pem(&contents, path)
                    .map_err(|e| e.to_string())?
                    .as_bytes()
                    .to_vec()
            }
            (Some(_), Some(_)) => {
                return Err("give one of --public-key or --public-key-file, not both".to_string())
            }
            (None, None) => {
                return Err(
                    "enrollment needs the node's signing key: --public-key <hex> or \
                     --public-key-file <node_identity.pub.pem>"
                        .to_string(),
                )
            }
        };

        if key.len() != 32 {
            return Err(format!(
                "Public key must be 32 bytes (Ed25519), got {}",
                key.len()
            ));
        }

        Ok(key)
    }

    /// Enroll a node: issue a CA credential and write its `nodes` registry row.
    ///
    /// Both halves are required. `occurrences.origin_node_id` references
    /// `nodes(node_id)`, so a credential without a registry row leaves the node
    /// unable to store a single occurrence.
    async fn ca_enroll(&self, req: EnrollRequest<'_>) -> Result<(), String> {
        use std::fs;

        // Validate the registry inputs first: a typo should not cost a credential
        // that then fails to enroll.
        let node_type =
            parse_node_type(req.node_type.unwrap_or(self.config.ca.node_type.as_str()))?;
        let validity_days = req.validity_days.unwrap_or(self.config.ca.validity_days);
        let location = self.enroll_location(req.lat, req.lon).await?;
        let owns_cells =
            resolve_owned_cells(req.owns_cells, &self.config.node.owns_cells, location)?;

        let db_url = self
            .database_for(req.database_url)
            .map_err(|e| format!("{e}, which enrollment needs to write the nodes registry row"))?;

        let key_path = self.ca_key_path(req.key_path);

        if !key_path.exists() {
            return Err(format!(
                "CA root key not found at {}. Run 'app ca-init' first.",
                key_path.display()
            ));
        }

        let ca = ca::CaRoot::load_from_file(&key_path)
            .map_err(|e| format!("Failed to load CA root key: {}", e))?;

        let public_key = Self::enroll_signing_key(req.public_key, req.public_key_file)?;

        // Issue credential
        let credential = ca
            .issue_credential(&public_key, Some(validity_days))
            .map_err(|e| format!("Failed to issue credential: {}", e))?;

        println!(
            "Credential issued for node_id: {}",
            hex::encode(&credential.node_id)
        );
        if let Some(name) = req.display_name {
            println!("Display name: {}", name);
        }
        println!("Valid until: {:?}", credential.expires_at);
        println!("CA root key: {}", key_path.display());

        // Output credential
        let credential_json = serde_json::to_string_pretty(&credential)
            .map_err(|e| format!("Failed to serialize credential: {}", e))?;

        if let Some(output_path) = req.output {
            fs::write(output_path, &credential_json)
                .map_err(|e| format!("Failed to write credential: {}", e))?;
            println!("Credential saved to {}", output_path);
        } else {
            println!("\n{}", credential_json);
        }

        // Registry row. Re-running enrollment after a failure here is safe: the
        // upsert replaces the row rather than conflicting with it.
        let pool = Pool::connect(&db_url)
            .await
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        // Stored as the full serialized credential: the bare CA signature would
        // not carry the validity window a verifier needs.
        let credential_bytes = serde_json::to_vec(&credential)
            .map_err(|e| format!("Failed to serialize credential for storage: {}", e))?;

        NodeRepository::register(
            &pool,
            &credential.node_id,
            node_type,
            &credential.signing_public_key,
            &credential_bytes,
            location,
            &owns_cells,
        )
        .await
        .map_err(|e| format!("Failed to write nodes registry row: {}", e))?;

        match location {
            Some((lat, lon)) => println!(
                "Registered node {} ({}) at {},{}",
                hex::encode(&credential.node_id),
                node_type_label(node_type),
                lat,
                lon
            ),
            None => println!(
                "Registered node {} ({}) with no location recorded",
                hex::encode(&credential.node_id),
                node_type_label(node_type)
            ),
        }

        // Report what was claimed rather than what was derived: the two differ
        // whenever the operator named cells, and ownership is the kind of thing a
        // surprise about is found late.
        if owns_cells.is_empty() {
            println!("Owns no res {} cell", repo::geo::RESOLUTION_MACRO);
        } else {
            let cells: Vec<String> = owns_cells.iter().map(CellIndex::to_string).collect();
            println!(
                "Owns {} res {} cell(s): {}",
                cells.len(),
                repo::geo::RESOLUTION_MACRO,
                cells.join(", ")
            );
        }

        Ok(())
    }

    /// Verify a node's credential
    async fn ca_verify(
        &self,
        node_id_hex: String,
        key_path: Option<String>,
        credential_file: Option<String>,
    ) -> Result<(), String> {
        use std::fs;

        let key_path = self.ca_key_path(key_path.as_deref());
        let node_id = decode_node_id(&node_id_hex)?;

        if !key_path.exists() {
            return Err(format!(
                "CA root key not found at {}. Run 'app ca-init' first.",
                key_path.display()
            ));
        }

        let ca = ca::CaRoot::load_from_file(&key_path)
            .map_err(|e| format!("Failed to load CA root key: {}", e))?;

        // Load credential from file or parse
        let Some(file_path) = credential_file else {
            return Err("Credential must be provided via --credential-file or database lookup not yet implemented".to_string());
        };
        let credential_json = fs::read_to_string(&file_path)
            .map_err(|e| format!("Failed to read credential file: {}", e))?;

        let credential: ca::Credential = serde_json::from_str(&credential_json)
            .map_err(|e| format!("Invalid credential JSON: {}", e))?;

        // The signature can only say the credential is intact; whether it is the
        // credential that was asked about is a separate question, and a stale
        // --credential-file would otherwise be reported as a pass for this node.
        if credential.node_id != node_id {
            return Err(format!(
                "{file_path} holds a credential for node {}, not {}",
                hex::encode(&credential.node_id),
                node_id_hex
            ));
        }

        // Verify credential
        match ca.verify_credential(&credential) {
            Ok(()) => {
                println!("✓ Credential is VALID");
                println!("  Node ID: {}", hex::encode(&credential.node_id));
                println!("  Issued: {}", credential.issued_at);
                if let Some(expires) = credential.expires_at {
                    println!("  Expires: {}", expires);
                } else {
                    println!("  Expires: never");
                }
                Ok(())
            }
            Err(e) => {
                println!("✗ Credential is INVALID: {}", e);
                Err(format!("Verification failed: {}", e))
            }
        }
    }

    /// Revoke a node's credentials
    ///
    /// The revocation goes into `node_revocations`, which is the ledger
    /// `ca ca-generate-rsl` reads when it builds a list, and the registry row is
    /// marked revoked in the same transaction. Marking the registry alone used to be
    /// the whole command, and it produced a CA that had "revoked" a node while
    /// publishing lists that never named it: no node can act on a revocation that
    /// exists only in a column the list builder does not read.
    async fn ca_revoke(
        &self,
        node_id_hex: String,
        reason: String,
        database_url: Option<String>,
    ) -> Result<(), String> {
        let node_id = decode_node_id(&node_id_hex)?;
        let reason_code = revocation_reason_code(&reason)?;

        let db_url = self.database_for(database_url.as_deref())?;

        let pool = sqlx::Pool::<sqlx::Postgres>::connect(&db_url)
            .await
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        // Revoking twice is not an error, and the second run must not quietly move the
        // date either. What an operator asking again needs to know is that the first
        // one happened, and whether anything has been told about it yet.
        if let Some(existing) = repo::RevocationRepository::get_revocation(&pool, &node_id)
            .await
            .map_err(|e| format!("Failed to read the revocation ledger: {e}"))?
        {
            println!(
                "Node {} was already revoked on {} ({}).",
                hex::encode(&node_id),
                existing.revoked_at,
                existing.reason_description()
            );
            if existing.rsl_sequence_number == 0 {
                println!(
                    "No node can see it yet: it is in the ledger and in no published list. \
                     Publish it with:\n  app ca ca-generate-rsl"
                );
            } else {
                println!(
                    "It has been published in list #{}, so nodes holding that list refuse this \
                     node's data.",
                    existing.rsl_sequence_number
                );
            }
            return Ok(());
        }

        // The ledger row names the key and the credential being revoked, and the
        // registry is where both live. A node that was never enrolled has nothing to
        // revoke: no credential is invalidated, and publishing a revocation of a key no
        // list ever carried is not a thing a node can do anything with.
        let registered = NodeRepository::find_by_id(&pool, &node_id)
            .await
            .map_err(|e| format!("Failed to read the nodes registry: {e}"))?
            .ok_or_else(|| {
                format!(
                    "{} has no row in the nodes table, so there is no credential to revoke. \
                     Either this node was never enrolled, or the registry is in another \
                     database — point --database-url at that one.",
                    hex::encode(&node_id)
                )
            })?;

        let recorded = repo::RevocationRepository::revoke_node(
            &pool,
            &repo::models::RevokedNode {
                node_id: node_id.clone(),
                revoked_at: chrono::Utc::now(),
                // NULL until a signed list carries it: there is no CA to attribute this
                // to yet, and publication is the act that attributes it.
                revoked_by: None,
                reason: reason_code,
                signing_public_key: registered.signing_public_key,
                ca_credential: registered.ca_credential,
                // 0 means "in no published list"; storing a list stamps the number of
                // the one that first carries it.
                rsl_sequence_number: 0,
                notes: None,
            },
        )
        .await
        .map_err(|e| format!("Failed to record the revocation: {e}"))?;

        println!(
            "Node {} revoked ({}), and its registry row marked revoked.",
            hex::encode(&node_id),
            recorded.reason_description()
        );
        println!(
            "That reaches nobody yet. A revocation only becomes visible to other nodes inside a \
             signed list:\n  app ca ca-generate-rsl"
        );
        println!(
            "The node's own signatures still verify — provenance is not authority. It is the \
             list that makes them untrustworthy."
        );

        Ok(())
    }

    /// Show CA information
    async fn ca_info(&self, key_path: Option<String>) -> Result<(), String> {
        let key_path = self.ca_key_path(key_path.as_deref());

        if !key_path.exists() {
            return Err(format!(
                "CA root key not found at {}. Run 'app ca-init' first.",
                key_path.display()
            ));
        }

        let ca = ca::CaRoot::load_from_file(&key_path)
            .map_err(|e| format!("Failed to load CA root key: {}", e))?;

        println!("=== CA Information ===");
        println!("Root key path: {}", key_path.display());
        println!("CA public key (hex): {}", hex::encode(ca.public_key()));
        println!("CA id (SHA-256 of the public key): {}", ca.ca_id_hex());
        println!(
            "To publish this CA's public key for other nodes: app ca ca-export-anchor \
             (writes the anchor as a .pub.pem beside the root key)"
        );

        Ok(())
    }

    /// Publish this CA's trust anchor beside its root key.
    ///
    /// The anchor is the CA public key in SubjectPublicKeyInfo PEM — everything a
    /// remote node needs to check this CA's credentials and revocation lists, and
    /// nothing it can forge with. This is the publishing half of M12: until an anchor
    /// exists, a node has no key to verify an RSL against and revocation cannot be
    /// turned on at all (B11).
    async fn ca_export_anchor(
        &self,
        key_path: Option<String>,
        output: Option<String>,
    ) -> Result<(), String> {
        let key_path = self.ca_key_path(key_path.as_deref());

        if !key_path.exists() {
            return Err(format!(
                "CA root key not found at {}. Run 'app ca ca-init' first.",
                key_path.display()
            ));
        }

        let ca = ca::CaRoot::load_from_file(&key_path)
            .map_err(|e| format!("Failed to load CA root key: {}", e))?;
        let anchor = ca.trust_anchor();

        let output = output
            .map(PathBuf::from)
            .unwrap_or_else(|| key_path.with_extension("pub.pem"));

        // An anchor file that names a different CA is not a thing to overwrite: every
        // node configured with it would stop accepting this CA's lists the moment the
        // file changed under them, and the operator would hear about it as a wave of
        // verification failures rather than from this command.
        if output.exists() {
            return match ca::TrustAnchor::load_from_file(&output) {
                Ok(existing) if existing == anchor => {
                    println!(
                        "Trust anchor at {} is already up to date (CA id {})",
                        output.display(),
                        anchor.ca_id_hex()
                    );
                    Ok(())
                }
                Ok(existing) => Err(format!(
                    "{} is a trust anchor for {}, not this CA ({}). Refusing to overwrite it; \
                     pass --output to publish this CA's anchor elsewhere.",
                    output.display(),
                    existing.ca_id_hex(),
                    anchor.ca_id_hex()
                )),
                Err(e) => Err(format!(
                    "{} exists and is not a readable trust anchor: {e}",
                    output.display()
                )),
            };
        }

        anchor
            .save_to_file(&output)
            .map_err(|e| format!("Failed to write trust anchor: {}", e))?;

        println!("=== Trust anchor published ===");
        println!("CA id: {}", anchor.ca_id_hex());
        println!(
            "Wrote: {} (world-readable: this is public material)",
            output.display()
        );
        println!("Give this file to every node that has to check this CA. It verifies and");
        println!("never signs, so copying it costs nothing in secrecy.");

        Ok(())
    }

    /// Convert a pre-PKCS#8 hex root key into a PKCS#8 PEM key file.
    ///
    /// The CA identity survives, because `ca_id` is derived from the public half:
    /// credentials and revocation lists published before the conversion still verify
    /// after it. That is what makes this a conversion rather than a re-enrollment of
    /// every node on the network.
    ///
    /// It does not delete the legacy file and does not touch the configuration. Two
    /// copies of a CA secret on one machine is exactly the state to avoid, but which
    /// copy is safe to remove depends on what the running node has loaded, and this
    /// command cannot know that.
    async fn ca_migrate_key(&self, from: String, output: Option<String>) -> Result<(), String> {
        let from = PathBuf::from(&from);

        // Already converted is a different problem from convertible, and needs a
        // different answer.
        if ca::CaRoot::load_from_file(&from).is_ok() {
            return Err(format!(
                "{} is already a PKCS#8 PEM key; there is nothing to convert. If the node \
                 will not start, check that [ca].key_path points at it.",
                from.display()
            ));
        }

        let ca = ca::CaRoot::load_legacy_hex_file(&from)
            .map_err(|e| format!("Failed to read the legacy root key: {}", e))?;

        let output = output
            .map(PathBuf::from)
            .unwrap_or_else(|| from.with_extension("pem"));

        if output.exists() {
            return Err(format!(
                "{} already exists; not overwriting it with a second copy of the key. Pass \
                 --output to choose another path.",
                output.display()
            ));
        }

        ca.save_to_file(&output, Some(0o600))
            .map_err(|e| format!("Failed to write the converted key: {}", e))?;

        println!("=== CA root key converted ===");
        println!("From:  {}", from.display());
        println!("To:    {} (PKCS#8 PEM, mode 0600)", output.display());
        println!("CA id: {} — unchanged by the conversion", ca.ca_id_hex());
        println!();
        println!(
            "Next: point [ca].key_path at {}, then delete {} once the node has started.",
            output.display(),
            from.display()
        );
        println!("Credentials and lists this CA published before the conversion still verify.");

        Ok(())
    }

    /// Generate a Revocation Status List (RSL)
    async fn ca_generate_rsl(
        &self,
        key_path: Option<String>,
        database_url: Option<String>,
        output: Option<String>,
        validity_days: u64,
    ) -> Result<(), String> {
        use std::fs;

        let key_path = self.ca_key_path(key_path.as_deref());

        if !key_path.exists() {
            return Err(format!(
                "CA root key not found at {}. Run 'app ca-init' first.",
                key_path.display()
            ));
        }

        let ca = ca::CaRoot::load_from_file(&key_path)
            .map_err(|e| format!("Failed to load CA root key: {}", e))?;

        // Get CA ID for the RSL
        let ca_id = ca.ca_id();

        // Connect to database and generate RSL
        let db_url = self
            .database_for(database_url.as_deref())
            .map_err(|e| format!("{e}, which RSL generation needs to read the revocations"))?;

        let pool = sqlx::Pool::<sqlx::Postgres>::connect(&db_url)
            .await
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        // The database holds the sequence counter. This command is the whole
        // publication — one process, run again next week — so a counter living
        // in it would restart at zero every time, which is the difference
        // between a list an old copy can be passed off as and one it cannot.
        let manager = ca::DatabaseRslManager::new(pool);

        // Generate reads the revocation ledger and numbers the list one past
        // the highest already published.
        let rsl_unsigned = manager
            .generate_rsl(&ca_id, validity_days)
            .await
            .map_err(|e| format!("Failed to generate RSL: {}", e))?;

        // Sign the list the manager produced. Rebuilding one here from just the
        // revocations would throw away the sequence number that was just
        // assigned to it.
        let rsl = ca
            .sign_rsl(rsl_unsigned)
            .map_err(|e| format!("Failed to sign RSL: {}", e))?;

        // Storing is what uses the number: publishing the same sequence twice
        // is refused, and the next list starts above it.
        manager
            .store_rsl(&rsl)
            .await
            .map_err(|e| format!("Failed to store RSL: {}", e))?;

        println!("=== RSL Generated ===");
        println!("Issuer: {}", hex::encode(&rsl.issuer_id));
        println!("Sequence: {}", rsl.sequence_number);
        println!("Revocations: {}", rsl.revocation_count());
        println!("Issued: {}", rsl.issued_at);
        println!("Expires: {}", rsl.expires_at);
        println!("Signature: {}", hex::encode(&rsl.signature));
        println!("Published to revocation_status_lists");

        // Output RSL
        let rsl_json = serde_json::to_string_pretty(&rsl)
            .map_err(|e| format!("Failed to serialize RSL: {}", e))?;

        if let Some(output_path) = output {
            fs::write(&output_path, &rsl_json)
                .map_err(|e| format!("Failed to write RSL: {}", e))?;
            println!("\nRSL saved to {}", output_path);
        } else {
            println!("\n{}", rsl_json);
        }

        Ok(())
    }
}

/// Parse a duration string (e.g., "1h", "30m", "7d")
fn parse_duration(s: &str) -> Result<chrono::Duration, String> {
    use chrono::Duration;

    let s = s.trim().to_lowercase();
    let mut chars = s.chars().peekable();

    let mut num = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            num.push(chars.next().unwrap());
        } else {
            break;
        }
    }

    let unit: String = chars.collect();
    let num: i64 = num.parse().map_err(|_| "Invalid number")?;

    match unit.as_str() {
        "s" | "sec" | "second" | "seconds" => Ok(Duration::seconds(num)),
        "m" | "min" | "minute" | "minutes" => Ok(Duration::minutes(num)),
        "h" | "hour" | "hours" => Ok(Duration::hours(num)),
        "d" | "day" | "days" => Ok(Duration::days(num)),
        _ => Err(format!("Unknown time unit: '{}'", unit)),
    }
}

/// Parse a `--node-id` hex string into the 32 bytes the registry stores.
fn decode_node_id(raw: &str) -> Result<Vec<u8>, String> {
    let node_id = hex::decode(raw).map_err(|e| format!("Invalid node ID hex {raw}: {e}"))?;

    if node_id.len() != 32 {
        return Err(format!(
            "Node ID must be 32 bytes (SHA-256), got {}",
            node_id.len()
        ));
    }

    Ok(node_id)
}

/// `--reason` as the ledger's integer code, 0-6 per the RFC 5280 adaptation the
/// schema documents.
///
/// Names rather than numbers because the reason lands in a record other people read
/// years later, and `4` typed at a terminal is not a considered choice between
/// policy violation and hold. Hyphens, underscores and case are all accepted; the
/// number is not, for the same reason.
fn revocation_reason_code(raw: &str) -> Result<i32, String> {
    let normalised = raw.trim().replace(['-', ' '], "_").to_lowercase();

    match normalised.as_str() {
        "unspecified" => Ok(0),
        "key_compromise" => Ok(1),
        "ca_compromise" => Ok(2),
        "ceased_operation" => Ok(3),
        "policy_violation" => Ok(4),
        "superseded" => Ok(5),
        "hold" => Ok(6),
        other => Err(format!(
            "unknown revocation reason '{other}'; expected one of: unspecified, key-compromise, \
             ca-compromise, ceased-operation, policy-violation, superseded, hold"
        )),
    }
}

/// Parse a `--node-type` value into the registry enum.
fn parse_node_type(raw: &str) -> Result<NodeType, String> {
    match raw.to_lowercase().as_str() {
        "full" => Ok(NodeType::Full),
        "light" => Ok(NodeType::Light),
        "aggregator" => Ok(NodeType::Aggregator),
        "signal" => Ok(NodeType::Signal),
        other => Err(format!(
            "Unknown node type '{}' (expected full, light, aggregator, or signal)",
            other
        )),
    }
}

/// Canonical label for a node type in command output.
fn node_type_label(node_type: NodeType) -> &'static str {
    match node_type {
        NodeType::Full => "full",
        NodeType::Light => "light",
        NodeType::Aggregator => "aggregator",
        NodeType::Signal => "signal",
    }
}

/// The location to record for an enrolled node.
///
/// `--lat`/`--lon` state it outright. Otherwise use whatever the position source
/// reports: the registry's `location` is where this node lives, and a node that
/// can locate itself has a better answer than NULL. A node with neither is
/// enrolled without a location rather than with a guessed one.
fn resolve_enroll_location(
    lat: Option<f64>,
    lon: Option<f64>,
    acquired: Option<(f64, f64)>,
) -> Result<Option<(f64, f64)>, String> {
    match (lat, lon) {
        (Some(lat), Some(lon)) => Ok(Some((lat, lon))),
        (None, None) => Ok(acquired),
        _ => Err("--lat and --lon must be given together".to_string()),
    }
}

/// The cells an enrolled node is recorded as owning.
///
/// A stated claim replaces a derived one entirely: a node told which cells it
/// owns is not also handed the one its antenna happens to sit in. Otherwise a
/// stationary node claims the res 6 cell it is standing in, and a node with no
/// location claims nothing — the honest answer for a mobile node, not an error.
fn resolve_owned_cells(
    flag: &[String],
    configured: &[String],
    location: Option<(f64, f64)>,
) -> Result<Vec<CellIndex>, String> {
    if !flag.is_empty() {
        return parse_cells_at(flag, "--owns-cell", repo::geo::RESOLUTION_MACRO);
    }
    if !configured.is_empty() {
        return parse_cells_at(configured, "[node] owns_cells", repo::geo::RESOLUTION_MACRO);
    }

    match location {
        Some((lat, lon)) => repo::geo::macro_cell(lat, lon)
            .map(|cell| vec![cell])
            .map_err(|e| {
                format!("{e} — cannot claim an owned cell for the node's location ({lat}, {lon})")
            }),
        None => Ok(Vec::new()),
    }
}

/// `--geo-cell`, parsed as strictly as the column it will be compared against.
fn parse_macro_cell(raw: &str) -> Result<CellIndex, String> {
    parse_cell_at(raw, "--geo-cell", repo::geo::RESOLUTION_MACRO)
}

fn parse_cells_at(
    raw: &[String],
    source: &str,
    resolution: Resolution,
) -> Result<Vec<CellIndex>, String> {
    raw.iter()
        .map(|value| parse_cell_at(value, source, resolution))
        .collect()
}

/// Parse one operator-spelled cell, refusing anything that is not at
/// `resolution`.
///
/// Postgres accepts an `h3index` of any resolution into a query or a column and
/// then matches nothing with it: the mistake shows up as "nobody covers this area"
/// rather than as an error, so it is caught here instead.
fn parse_cell_at(raw: &str, source: &str, resolution: Resolution) -> Result<CellIndex, String> {
    let cell = repo::geo::parse_cell(raw).map_err(|e| format!("{source}: {e}"))?;

    if cell.resolution() != resolution {
        let suggestion = match repo::geo::parent_cell(cell, resolution) {
            Ok(parent) => format!(", did you mean {parent}?"),
            Err(_) => String::new(),
        };

        return Err(format!(
            "{source}: {raw} is a resolution {} cell; expected resolution {resolution}{suggestion}",
            cell.resolution()
        ));
    }

    Ok(cell)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Args {
        Args::try_parse_from(argv).expect("test argv should parse")
    }

    #[test]
    fn parse_node_type_accepts_known_types_case_insensitively() {
        assert_eq!(parse_node_type("full").unwrap(), NodeType::Full);
        assert_eq!(parse_node_type("FULL").unwrap(), NodeType::Full);
        assert_eq!(parse_node_type("Light").unwrap(), NodeType::Light);
        assert_eq!(parse_node_type("aggregator").unwrap(), NodeType::Aggregator);
        assert_eq!(parse_node_type("signal").unwrap(), NodeType::Signal);
    }

    #[test]
    fn parse_node_type_rejects_unknown_types() {
        let err = parse_node_type("supernode").unwrap_err();
        assert!(err.contains("supernode"), "unexpected message: {}", err);
    }

    #[test]
    fn revocation_reasons_are_named_not_typed() {
        assert_eq!(revocation_reason_code("unspecified").unwrap(), 0);
        assert_eq!(revocation_reason_code("key-compromise").unwrap(), 1);
        assert_eq!(revocation_reason_code("  Policy_Violation ").unwrap(), 4);
        assert_eq!(revocation_reason_code("HOLD").unwrap(), 6);

        // The reason is stored in a record somebody reads years later, so a bare
        // number is refused rather than guessed at — and the refusal lists what it
        // will take, in the same spelling the help uses.
        let err = revocation_reason_code("4").unwrap_err();
        assert!(err.contains("key-compromise"), "{err}");
        assert!(revocation_reason_code("superseded yesterday").is_err());
    }

    #[test]
    fn revocation_takes_a_reason_and_no_root_key() {
        let revoke = |argv: &[&str]| match parse(argv).command {
            Some(Commands::Ca(CaCommands::CaRevoke {
                node_id, reason, ..
            })) => (node_id, reason),
            other => panic!("expected ca revoke, got {other:?}"),
        };

        let id = "ab".repeat(32);

        let (_, reason) = revoke(&["app", "ca", "ca-revoke", "--node-id", &id]);
        assert_eq!(
            reason, "unspecified",
            "the command must not invent a reason the operator did not give"
        );

        let (node_id, reason) = revoke(&[
            "app",
            "ca",
            "ca-revoke",
            "--node-id",
            &id,
            "--reason",
            "key-compromise",
        ]);
        assert_eq!(node_id, id);
        assert_eq!(reason, "key-compromise");

        // The root key belongs to publication, not to recording. Carrying a `--key-path`
        // nobody read is how `ca-revoke` could report success while publishing nothing
        // attributable, so it is now an error rather than an ignored flag.
        assert!(
            Args::try_parse_from([
                "app",
                "ca",
                "ca-revoke",
                "--node-id",
                &id,
                "--key-path",
                "/tmp/root_key.pem"
            ])
            .is_err(),
            "--key-path no longer means anything here"
        );
    }

    #[test]
    fn a_node_id_has_to_be_thirty_two_bytes_of_hex() {
        let hex_id = "a".repeat(64);
        assert_eq!(decode_node_id(&hex_id).unwrap().len(), 32);

        // Both ways to get it wrong should say what was wrong, quoting the value
        // the operator actually typed.
        let not_hex = decode_node_id("zz").unwrap_err();
        assert!(not_hex.contains("zz"), "got: {not_hex}");

        let too_short = decode_node_id("abcd").unwrap_err();
        assert!(too_short.contains("32 bytes"), "got: {too_short}");
    }

    #[test]
    fn an_unadorned_invocation_states_no_flags_at_all() {
        // The invariant the precedence rule depends on: anything the operator did
        // not type has to stay None, or the config file can never win.
        let flags = parse(&["app"]).flags();

        assert!(flags.database_url.is_none());
        assert!(flags.log_level.is_none());
        assert!(flags.node_id.is_none());
        assert!(flags.rate_limit_ms.is_none());
        assert!(flags.scan_interval_ms.is_none());
        assert!(flags.store_raw_payload.is_none());
        assert!(flags.use_mock_backend.is_none());
        assert!(flags.fixed_location.is_none());
        assert!(flags.location_mode.is_none());
    }

    #[test]
    fn a_bare_boolean_flag_reads_as_true_rather_than_needing_a_value() {
        let flags = parse(&["app", "--use-mock-backend", "--store-raw-payload"]).flags();

        assert_eq!(flags.use_mock_backend, Some(true));
        assert_eq!(flags.store_raw_payload, Some(true));
    }

    #[test]
    fn a_bare_boolean_says_no_when_the_value_is_attached_and_false() {
        // "off" has to be expressible from the command line, or the flag layer
        // could only ever turn a setting on.
        let flags = parse(&[
            "app",
            "--store-raw-payload=false",
            "--use-mock-backend=false",
        ])
        .flags();

        assert_eq!(flags.store_raw_payload, Some(false));
        assert_eq!(flags.use_mock_backend, Some(false));
    }

    #[test]
    fn a_bare_boolean_does_not_swallow_the_subcommand_after_it() {
        // `app --use-mock-backend ca …` must still be the `ca` subcommand, not a
        // mock backend called "ca".
        let args = parse(&["app", "--use-mock-backend", "ca", "ca-init"]);

        assert_eq!(args.use_mock_backend, Some(true));
        assert!(
            matches!(args.command(), Commands::Ca(CaCommands::CaInit { .. })),
            "unexpected command: {:?}",
            args.command
        );
    }

    #[test]
    fn the_position_flags_reach_the_flag_layer() {
        let flags = parse(&[
            "app",
            "--location-mode",
            "gps",
            "--gps-backend",
            "mock",
            "--gps-host",
            "127.0.0.1",
            "--gps-port",
            "2950",
            "--gps-timeout-ms",
            "500",
            "--fixed-location",
            "40.6892,-74.0445",
        ])
        .flags();

        assert_eq!(flags.location_mode.as_deref(), Some("gps"));
        assert_eq!(flags.gps_backend.as_deref(), Some("mock"));
        assert_eq!(flags.gps_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(flags.gps_port, Some(2950));
        assert_eq!(flags.gps_timeout_ms, Some(500));
        assert_eq!(flags.fixed_location.as_deref(), Some("40.6892,-74.0445"));
    }

    #[test]
    fn the_database_flags_are_all_carried() {
        let flags = parse(&[
            "app",
            "--pg-host",
            "db.internal",
            "--pg-port",
            "6543",
            "--pg-database",
            "travel",
            "--pg-user",
            "btmon",
            "--pg-password",
            "secret",
        ])
        .flags();

        assert_eq!(flags.pg_host.as_deref(), Some("db.internal"));
        assert_eq!(flags.pg_port, Some(6543));
        assert_eq!(flags.pg_database.as_deref(), Some("travel"));
        assert_eq!(flags.pg_user.as_deref(), Some("btmon"));
        assert_eq!(flags.pg_password.as_deref(), Some("secret"));
    }

    #[test]
    fn no_subcommand_is_the_monitor_and_inherits_the_global_node_id() {
        let args = parse(&["app", "--node-id", "from-global"]);

        assert!(matches!(
            args.command(),
            Commands::Monitor { node_id: Some(id) } if id == "from-global"
        ));
    }

    #[test]
    fn a_node_id_given_to_monitor_wins_over_the_global_one() {
        let args = parse(&[
            "app",
            "--node-id",
            "from-global",
            "monitor",
            "--node-id",
            "from-sub",
        ]);

        assert!(matches!(
            args.command(),
            Commands::Monitor { node_id: Some(id) } if id == "from-sub"
        ));
    }

    #[test]
    fn a_node_id_given_to_monitor_reaches_the_configuration_layer() {
        // The configuration is resolved from the flag layer, so a node id that
        // arrives only in the subcommand value is read off the command line and
        // then dropped. Both spellings have to end up in the same place.
        let after_monitor = parse(&["app", "monitor", "--node-id", "from-sub"]);
        assert_eq!(after_monitor.flags().node_id.as_deref(), Some("from-sub"));

        let before_monitor = parse(&["app", "--node-id", "from-global", "monitor"]);
        assert_eq!(
            before_monitor.flags().node_id.as_deref(),
            Some("from-global")
        );

        let both = parse(&[
            "app",
            "--node-id",
            "from-global",
            "monitor",
            "--node-id",
            "from-sub",
        ]);
        assert_eq!(both.flags().node_id.as_deref(), Some("from-sub"));
    }

    #[test]
    fn ca_subcommands_are_reachable_and_their_flags_stay_unresolved() {
        // `--validity-days` and `--node-type` must arrive as None when absent, so
        // `[ca].validity_days` and `[ca].node_type` can supply them.
        let args = parse(&["app", "ca", "ca-enroll", "--public-key", "abcd"]);

        match args.command() {
            Commands::Ca(CaCommands::CaEnroll {
                public_key,
                public_key_file,
                validity_days,
                node_type,
                key_path,
                ..
            }) => {
                assert_eq!(public_key.as_deref(), Some("abcd"));
                assert!(public_key_file.is_none());
                assert_eq!(validity_days, None);
                assert_eq!(node_type, None);
                assert_eq!(key_path, None);
            }
            other => panic!("expected ca enroll, got {other:?}"),
        }
    }

    /// A node writes its key as `node_identity.pub.pem`; that file, not a copy of its
    /// contents pasted into a terminal, is how it should reach the CA.
    #[test]
    fn enrollment_reads_the_node_key_from_a_file_or_from_hex() {
        let dir = tempfile::TempDir::new().unwrap();
        let key = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
        let path = dir.path().join("node_identity.pub.pem");
        std::fs::write(
            &path,
            ca::pemkeys::write_public_key_pem(&key.verifying_key()),
        )
        .unwrap();

        let from_file = Cli::enroll_signing_key(None, Some(path.to_str().unwrap())).unwrap();
        assert_eq!(from_file, key.verifying_key().as_bytes().to_vec());

        let from_hex =
            Cli::enroll_signing_key(Some(&hex::encode(from_file.clone())), None).unwrap();
        assert_eq!(from_hex, from_file);

        // Two answers is a contradiction, not a tiebreak; no answer is an enrollment
        // that would otherwise issue a credential for a key nobody named.
        assert!(Cli::enroll_signing_key(Some("ab"), Some(path.to_str().unwrap())).is_err());
        assert!(Cli::enroll_signing_key(None, None).is_err());
        // A short hex string parses, so the length check has to be the one that says
        // no — the same is not true of the PEM form, which cannot be short.
        assert!(Cli::enroll_signing_key(Some("abcd"), None).is_err());
    }

    #[test]
    fn explicit_coordinates_beat_an_acquired_position() {
        let resolved =
            resolve_enroll_location(Some(1.0), Some(2.0), Some((40.6892, -74.0445))).unwrap();

        assert_eq!(resolved, Some((1.0, 2.0)));
    }

    #[test]
    fn an_acquired_position_is_registered_when_the_flags_are_silent() {
        let resolved = resolve_enroll_location(None, None, Some((40.6892, -74.0445))).unwrap();

        assert_eq!(resolved, Some((40.6892, -74.0445)));
    }

    #[test]
    fn a_node_with_no_flag_and_no_fix_enrolls_without_a_location() {
        assert_eq!(resolve_enroll_location(None, None, None).unwrap(), None);
    }

    #[test]
    fn half_a_location_is_rejected_even_when_a_position_is_available() {
        assert!(resolve_enroll_location(Some(1.0), None, Some((1.0, 2.0))).is_err());
        assert!(resolve_enroll_location(None, Some(1.0), None).is_err());
    }

    /// Statue of Liberty, res 6 — `repo::geo`'s pinned fixture.
    const LIBERTY_MACRO: &str = "862a1072fffffff";
    /// The same place at res 9, which is not ownable.
    const LIBERTY_FINE: &str = "892a1072b5bffff";

    fn cells(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn a_stationary_node_claims_the_cell_it_stands_in() {
        let claimed = resolve_owned_cells(&[], &[], Some((40.6892, -74.0445))).unwrap();

        assert_eq!(claimed, vec![repo::geo::parse_cell(LIBERTY_MACRO).unwrap()]);
    }

    #[test]
    fn a_node_with_no_location_claims_nothing() {
        assert_eq!(
            resolve_owned_cells(&[], &[], None).unwrap(),
            Vec::<CellIndex>::new()
        );
    }

    #[test]
    fn a_stated_claim_replaces_the_cell_derived_from_the_location() {
        let somewhere_else = "860326237ffffff";

        let claimed =
            resolve_owned_cells(&cells(&[somewhere_else]), &[], Some((40.6892, -74.0445))).unwrap();

        assert_eq!(
            claimed,
            vec![repo::geo::parse_cell(somewhere_else).unwrap()]
        );
    }

    #[test]
    fn the_configured_claim_is_used_when_no_flag_names_a_cell() {
        let claimed = resolve_owned_cells(&[], &cells(&[LIBERTY_MACRO]), Some((0.0, 0.0))).unwrap();

        assert_eq!(claimed, vec![repo::geo::parse_cell(LIBERTY_MACRO).unwrap()]);
    }

    #[test]
    fn the_flag_claim_beats_the_configured_one() {
        let claimed =
            resolve_owned_cells(&cells(&["860326237ffffff"]), &cells(&[LIBERTY_MACRO]), None)
                .unwrap();

        assert_eq!(
            claimed,
            vec![repo::geo::parse_cell("860326237ffffff").unwrap()]
        );
    }

    #[test]
    fn hex_and_decimal_spell_the_same_claim() {
        // `Display` on a cell is the hex spelling, so the decimal form has to come
        // from the integer the cell is — which is exactly what an operator pasting
        // from a spreadsheet would type.
        let decimal = u64::from(repo::geo::parse_cell(LIBERTY_MACRO).unwrap()).to_string();

        assert_eq!(
            resolve_owned_cells(&cells(&[&decimal]), &[], None).unwrap(),
            resolve_owned_cells(&cells(&[LIBERTY_MACRO]), &[], None).unwrap()
        );
    }

    #[test]
    fn a_claim_at_the_wrong_resolution_is_refused_and_the_parent_suggested() {
        let error = resolve_owned_cells(&cells(&[LIBERTY_FINE]), &[], None).unwrap_err();

        assert!(error.contains(LIBERTY_FINE), "unexpected message: {error}");
        assert!(error.contains("--owns-cell"), "unexpected message: {error}");
        assert!(
            error.contains(LIBERTY_MACRO),
            "should suggest the res 6 parent: {error}"
        );
    }

    #[test]
    fn a_bad_configured_cell_blames_the_setting_not_the_command_line() {
        let error = resolve_owned_cells(&[], &cells(&["not-a-cell"]), None).unwrap_err();

        assert!(
            error.contains("[node] owns_cells"),
            "unexpected message: {error}"
        );
        assert!(
            !error.contains("--owns-cell"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn owns_cell_can_be_given_more_than_once() {
        let args = parse(&[
            "app",
            "ca",
            "ca-enroll",
            "--public-key",
            "abcd",
            "--owns-cell",
            LIBERTY_MACRO,
            "--owns-cell",
            "860326237ffffff",
        ]);

        match args.command() {
            Commands::Ca(CaCommands::CaEnroll { owns_cell, .. }) => {
                assert_eq!(owns_cell, vec![LIBERTY_MACRO, "860326237ffffff"]);
            }
            other => panic!("expected ca enroll, got {other:?}"),
        }
    }

    #[test]
    fn an_enrollment_without_owns_cell_states_no_claim() {
        let args = parse(&["app", "ca", "ca-enroll", "--public-key", "abcd"]);

        match args.command() {
            Commands::Ca(CaCommands::CaEnroll { owns_cell, .. }) => {
                assert!(
                    owns_cell.is_empty(),
                    "an absent flag must not look like a claim"
                );
            }
            other => panic!("expected ca enroll, got {other:?}"),
        }
    }

    #[test]
    fn geo_cell_takes_the_spelling_psql_prints() {
        let args = parse(&["app", "query", "--geo-cell", LIBERTY_MACRO]);

        match args.command() {
            Commands::Query { geo_cell, .. } => {
                assert_eq!(
                    geo_cell,
                    Some(repo::geo::parse_cell(LIBERTY_MACRO).unwrap())
                );
            }
            other => panic!("expected query, got {other:?}"),
        }
    }

    #[test]
    fn geo_cell_at_the_wrong_resolution_is_refused_at_parse_time() {
        // `geo_cell_macro = <a res 9 cell>` is a perfectly valid query that can
        // only ever return nothing, which reads as "this area was quiet".
        let error = Args::try_parse_from(["app", "query", "--geo-cell", LIBERTY_FINE])
            .expect_err("a res 9 cell cannot match the res 6 column being queried");
        let message = error.to_string();

        assert!(
            message.contains(LIBERTY_MACRO),
            "should suggest the res 6 parent: {message}"
        );
    }

    #[test]
    fn an_anchor_export_states_no_paths_unless_it_is_told_both() {
        // The default output is the root key's path with a `.pub.pem` extension, which
        // only makes sense once `[ca].key_path` has been resolved — so clap must not
        // fill it in, or it would outrank the config file the same way any
        // `default_value` would.
        match parse(&["app", "ca", "ca-export-anchor"]).command() {
            Commands::Ca(CaCommands::CaExportAnchor { key_path, output }) => {
                assert!(key_path.is_none(), "got: {key_path:?}");
                assert!(output.is_none(), "got: {output:?}");
            }
            other => panic!("expected ca export-anchor, got {other:?}"),
        }
    }

    #[test]
    fn an_anchor_export_takes_both_paths_when_given() {
        match parse(&[
            "app",
            "ca",
            "ca-export-anchor",
            "--key-path",
            "/etc/btmon/root_key.pem",
            "--output",
            "/srv/www/ca.pub.pem",
        ])
        .command()
        {
            Commands::Ca(CaCommands::CaExportAnchor { key_path, output }) => {
                assert_eq!(key_path.as_deref(), Some("/etc/btmon/root_key.pem"));
                assert_eq!(output.as_deref(), Some("/srv/www/ca.pub.pem"));
            }
            other => panic!("expected ca export-anchor, got {other:?}"),
        }
    }

    #[test]
    fn a_key_conversion_names_the_file_it_converts_and_nothing_else() {
        let error = Args::try_parse_from(["app", "ca", "ca-migrate-key"])
            .expect_err("converting without saying what to convert is nothing to do");
        assert!(
            error.to_string().contains("--from"),
            "should name the missing flag: {error}"
        );

        match parse(&[
            "app",
            "ca",
            "ca-migrate-key",
            "--from",
            "/var/lib/ca/root_key.hex",
        ])
        .command()
        {
            Commands::Ca(CaCommands::CaMigrateKey { from, output }) => {
                assert_eq!(from, "/var/lib/ca/root_key.hex");
                assert!(
                    output.is_none(),
                    "the .pem beside the input is decided after the config is resolved"
                );
            }
            other => panic!("expected ca migrate-key, got {other:?}"),
        }
    }
}
