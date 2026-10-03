//! Bluetooth Monitoring Application
//!
//! This application monitors Bluetooth Low Energy (BLE) devices and stores
//! discoveries in a PostgreSQL database with cryptographic provenance.
//!
//! # Features
//!
//! - **Cryptographic Signing**: All occurrences are signed with Ed25519
//! - **Rate Limiting**: Deduplicates device observations (default: 15s threshold)
//! - **Node Identity**: Persistent node identity with automatic key generation
//! - **Database Storage**: PostgreSQL with PostGIS support
//! - **Position Acquisition**: A fixed point or a GPS receiver, per occurrence
//!
//! # Configuration
//!
//! Every setting is taken from the first layer that states it:
//!
//! ```text
//! flag  >  config file  >  environment  >  built-in default
//! ```
//!
//! The environment layer is the process environment first, then a dotenv file
//! (`.env` in the working directory, else `.env.database`; name another with
//! `ENV_FILE`). The config file is only read when `--config-file` or
//! `$CONFIG_FILE` names it — nothing is discovered implicitly.
//!
//! Because a flag that was not typed has to stay distinguishable from one that
//! was, no flag carries a clap default: `--rate-limit-ms 15000` beats the file,
//! and an omitted flag lets the file decide.
//!
//! See `config.example.toml` at the repository root for the full annotated file,
//! including `[location]` and `[ca]`. The table below lists the flag, its setting
//! in that file, and the variable underneath.
//!
//! ## Database
//!
//! - `-d, --database-url` / `[database].url` / `DATABASE_URL`: connection string
//! - `--pg-host` / `[database].host` / `PGHOST`: default: localhost
//! - `--pg-port` / `[database].port` / `PGPORT`: default: 5432
//! - `--pg-database` / `[database].database` / `PGDATABASE`: required if no URL
//! - `--pg-user` / `[database].user` / `PGUSER`: required if no URL
//! - `--pg-password` / `[database].password` / `PGPASSWORD`
//!
//! ## Node and logging
//!
//! - `--node-id` / `[node].id` / `NODE_ID`: the node id this node has to turn out to
//!   be — 64 hex characters, the SHA-256 of its signing key. Optional. It asserts an
//!   identity rather than choosing one: startup stops when the key in the data
//!   directory disagrees, and prints both ids.
//! - `--log-level` / `[log].level` / `LOG_LEVEL`: default: info
//!
//! ## Bluetooth
//!
//! - `--adapter-id` / `[bluetooth].adapter_id` / `BT_ADAPTER_ID`: which radio to
//!   open. Matched case-insensitively against the adapter description or its first
//!   word (`hci1` in `hci1 (usb:1d6b:0003)`); one that matches nothing, or matches
//!   two adapters, stops startup instead of picking. Default: the first adapter.
//! - `--scan-interval-ms` / `[bluetooth].scan_interval_ms` / `BT_SCAN_INTERVAL_MS`: default: 1000.
//!   Scanning is continuous; this is how often the monitor re-arms it, and how often
//!   the simulated radio advertises.
//! - `--data-dir` / `[bluetooth].data_dir` / `BT_DATA_DIR`: default: ~/.btmon/data
//! - `--rate-limit-ms` / `[bluetooth].rate_limit_ms` / `BT_RATE_LIMIT_MS`: default: 15000
//! - `--stream-reopen-delay-ms` / `[bluetooth].stream_reopen_delay_ms` / `BT_STREAM_REOPEN_DELAY_MS`: default: 1000
//! - `--store-raw-payload` / `[bluetooth].store_raw_payload` / `BT_STORE_RAW_PAYLOAD`: default: true.
//!   Bare flag means yes, `=false` means no. Keeps the radio's own advertisement
//!   bytes as `signal_payload.ble.raw_payload_hex`, inside the signed payload. A
//!   backend that hands over only decoded properties stores nothing extra and says
//!   so once.
//! - `--use-mock-backend` / `[bluetooth].use_mock_backend` / `BT_USE_MOCK_BACKEND`: needs the `mock`
//!   feature, and a binary built without it refuses to start rather than falling
//!   back to the real adapter. The simulated radio advertises five beacons, so an
//!   unattended run has something to discover and rows to store.
//!
//! ## Location
//!
//! - `--location-mode` / `[location].mode` / `BT_LOCATION_MODE`: `auto`, `fixed`, `gps`, `off`
//! - `--fixed-location` / `[location].fixed` / `BT_LOCATION_FIXED`: "lat,lon"
//! - `--gps-backend` / `[location.gps].backend` / `BT_GPS_BACKEND`: `gpsd` or `mock`
//! - `--gps-host` / `[location.gps].host` / `BT_GPS_HOST`: default: 127.0.0.1
//! - `--gps-port` / `[location.gps].port` / `BT_GPS_PORT`: default: 2947
//! - `--gps-timeout-ms` / `[location.gps].timeout_ms` / `BT_GPS_TIMEOUT_MS`: default: 2000
//!
//! `[bluetooth].fixed_location` and `BT_FIXED_LOCATION` are deprecated names for
//! `[location].fixed`; they still work and log a note pointing at the new one.
//!
//! ## CA
//!
//! Defaults for the `ca` subcommands, each of which still takes its own flag:
//!
//! - `[ca].key_path` / `CA_ROOT_KEY_PATH`: default: /var/lib/btmon/ca/root_key.hex
//! - `[ca].validity_days` / `CA_VALIDITY_DAYS`: default: 90
//! - `[ca].node_type` / `CA_NODE_TYPE` / default: full
//!
//! # Example
//!
//! ```bash
//! # From a config file
//! cargo run -- --config-file config.toml
//!
//! # Or entirely from the environment
//! DATABASE_URL=postgres://localhost:5432/btmon cargo run
//!
//! # The same node, asserting the identity it is enrolled under
//! cargo run -- -d postgres://localhost:5432/btmon \
//!   --node-id 0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0
//!
//! # A stationary node, no receiver
//! cargo run -- --config-file config.toml --fixed-location "40.6892,-74.0445"
//!
//! # A node that reads gpsd on the host
//! cargo run -- --config-file config.toml --location-mode gps --gps-host 172.17.0.1
//! ```

mod app;
mod cli;
mod config;
mod error;
mod identity;
mod node;
mod position;
mod provenance;

use std::process::ExitCode;

use app::App;
use cli::{Cli, Commands};
use log::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    // Parse argv and collapse the configuration layers. This runs before the
    // logger exists because the resolved log level is one of the things it
    // decides, so problems are printed rather than logged.
    let cli = match Cli::from_args() {
        Ok(cli) => cli,
        Err(problems) => {
            for problem in problems {
                eprintln!("configuration error: {problem}");
            }
            return ExitCode::FAILURE;
        }
    };

    // Initialize logging early to catch startup errors
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(&cli.config.log.level),
    )
    .init();

    for note in &cli.config_notes {
        info!("Configuration: {}", note);
    }

    // CA and database subcommands act on their own and must not start a
    // monitoring session; only the monitor - the default when no subcommand is
    // given - falls through to the application below.
    if !matches!(cli.command, Commands::Monitor { .. }) {
        return match cli.run_command().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                error!("Command failed: {}", e);
                ExitCode::FAILURE
            }
        };
    }

    info!("Starting Bluetooth monitoring application");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));

    // Print structured configuration, including which files were read.
    info!("Configuration:\n{}", cli.config.pretty_print());

    // Create and run application
    match App::new(cli.config).await {
        Ok(mut app) => {
            // Print node info
            info!("Node ID: {}", hex::encode(app.node_id()));
            info!("Data directory: {:?}", app.data_dir());

            match app.run().await {
                Ok(()) => {
                    // Print final statistics
                    app.print_stats();
                    info!("Application completed successfully");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    error!("Application error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            error!("Failed to initialize application: {}", e);
            ExitCode::FAILURE
        }
    }
}
