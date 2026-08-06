//! Bluetooth Monitoring Application
//!
//! This application monitors Bluetooth Low Energy (BLE) devices and stores
//! discoveries in a PostgreSQL database.
//!
//! # Configuration
//!
//! Configuration is done via command-line arguments or environment variables:
//!
//! - `DATABASE_URL` / `-d, --database-url`: PostgreSQL connection string (required)
//! - `NODE_ID` / `--node-id`: Node UUID for identification (required)
//! - `LOG_LEVEL` / `--log-level`: Log level (default: info)
//! - `BT_SCAN_INTERVAL_MS` / `--scan-interval-ms`: Scan interval (default: 1000)
//! - `BT_STORE_RAW_PAYLOAD` / `--store-raw-payload`: Store raw payloads (default: true)
//! - `BT_ADAPTER_ID` / `--adapter-id`: Bluetooth adapter ID (optional)
//!
//! # Example
//!
//! ```bash
//! DATABASE_URL=postgres://localhost:7789/travel NODE_ID=550e8400-e29b-41d4-a716-446655440000 cargo run
//! ```

mod app;
mod config;
mod error;

use std::process::ExitCode;

use app::App;
use config::Config;
use log::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    // Parse configuration
    let config = Config::parse_args();

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
            match app.run().await {
                Ok(()) => {
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
