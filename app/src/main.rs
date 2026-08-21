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
//!
//! # Configuration
//!
//! Configuration is done via command-line arguments or environment variables:
//!
//! ## Database
//!
//! - `DATABASE_URL` / `-d, --database-url`: PostgreSQL connection string (required)
//! - `PGHOST` / `--pg-host`: PostgreSQL host (default: localhost)
//! - `PGPORT` / `--pg-port`: PostgreSQL port (default: 5432)
//! - `PGDATABASE` / `--pg-database`: Database name (required if no DATABASE_URL)
//! - `PGUSER` / `--pg-user`: Database user (required if no DATABASE_URL)
//! - `PGPASSWORD` / `--pg-password`: Database password
//!
//! ## Node Configuration
//!
//! - `NODE_ID` / `--node-id`: Node UUID for identification (required)
//! - `BT_DATA_DIR` / `--data-dir`: Data directory for node identity (default: ~/.btmon/data)
//! - `BT_RATE_LIMIT_MS` / `--rate-limit-ms`: Rate limit threshold in ms (default: 15000)
//! - `BT_FIXED_LOCATION` / `--fixed-location`: Fixed location "lat,lon" (optional)
//!
//! ## Logging
//!
//! - `LOG_LEVEL` / `--log-level`: Log level (default: info)
//!
//! ## Bluetooth
//!
//! - `BT_SCAN_INTERVAL_MS` / `--scan-interval-ms`: Scan interval (default: 1000)
//! - `BT_ADAPTER_ID` / `--adapter-id`: Bluetooth adapter ID (optional)
//!
//! # Example
//!
//! ```bash
//! # Using DATABASE_URL
//! DATABASE_URL=postgres://localhost:5432/btmon NODE_ID=550e8400-e29b-41d4-a716-446655440000 cargo run
//!
//! # Using individual parameters
//! PGHOST=localhost PGDATABASE=btmon PGUSER=postgres NODE_ID=550e8400-e29b-41d4-a716-446655440000 \
//!   cargo run -- --pg-password secretpass
//!
//! # With fixed location (e.g., for a stationary node)
//! DATABASE_URL=postgres://localhost:5432/btmon NODE_ID=550e8400-e29b-41d4-a716-446655440000 \
//!   BT_FIXED_LOCATION="40.6892,-74.0445" cargo run
//! ```

mod app;
mod cli;
mod config;
mod error;
mod node;
mod provenance;

use std::path::PathBuf;
use std::process::ExitCode;

use app::App;
use cli::Cli;
use config::Config;
use log::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    // Parse command-line arguments (handles config file loading)
    let cli = Cli::parse();

    // Load configuration from file if specified
    let config = if let Some(ref config_file_path) = cli.config_file {
        match Config::load_from_file(&PathBuf::from(config_file_path)) {
            Ok(Some(file_config)) => {
                info!("Loaded configuration from: {}", config_file_path);
                cli.config.clone().merge_with_file(file_config)
            }
            Ok(None) => {
                info!("No config file found at: {}", config_file_path);
                cli.config.clone()
            }
            Err(e) => {
                eprintln!("Error loading config file: {}", e);
                return ExitCode::FAILURE;
            }
        }
    } else {
        cli.config.clone()
    };

    // Initialize logging early to catch startup errors
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(&config.log_level)
    )
    .init();

    info!("Starting Bluetooth monitoring application");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));

    // Create and run application
    match App::new(config).await {
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
