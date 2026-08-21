//! Main application state and event loop.
//!
//! This module provides the `App` struct which integrates the FullNode
//! with the application lifecycle.

use bt_mon::{create_btleplug_monitor, DeviceMonitor};
use log::{error, info};
use std::path::PathBuf;

use crate::config::Config;
use crate::error::{AppError, Result};
use crate::node::full::{FullNode, FullNodeConfig};

/// Main application state.
///
/// This struct wraps the FullNode and manages the application lifecycle.
pub struct App {
    node: FullNode,
    data_dir: PathBuf,
}

impl App {
    /// Create a new application instance.
    ///
    /// This initializes the FullNode with the provided configuration,
    /// including database connection, node identity, and rate limiting.
    pub async fn new(config: Config) -> Result<Self> {
        // Validate configuration
        config.validate()?;

        // Build database URL from config (either DSN or components)
        let database_url = config.database_url().map_err(AppError::from)?;

        info!("Connecting to database...");
        let pool = repo::Pool::connect(&database_url)
            .await
            .map_err(AppError::Database)?;
        info!("Connected to database");

        // Get data directory
        let data_dir = config.data_dir().map_err(AppError::from)?;
        info!("Data directory: {:?}", data_dir);

        // Create data directory if it doesn't exist
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| AppError::Io(format!("Failed to create data directory: {}", e)))?;

        // Parse fixed location if provided
        let fixed_location = match config.fixed_location() {
            Some(Ok((lat, lon))) => Some((lat, lon)),
            Some(Err(e)) => return Err(AppError::Config(e)),
            None => None,
        };

        // Build FullNode configuration
        let fullnode_config = FullNodeConfig {
            pool,
            data_dir: data_dir.clone(),
            rate_limit_threshold_ms: config.rate_limit_ms,
            rate_limit_max_cache_size: None, // Could be configurable
            fixed_location,
        };

        // Create FullNode instance
        info!("Initializing FullNode...");
        let node = FullNode::new(fullnode_config).await?;
        info!("FullNode initialized with node ID: {}", hex::encode(node.node_id()));

        Ok(Self { node, data_dir })
    }

    /// Run the main event loop.
    ///
    /// This creates a Bluetooth monitor and delegates to the FullNode's
    /// run() method which handles Bluetooth monitoring, occurrence signing,
    /// and storage.
    pub async fn run(&mut self) -> Result<()> {
        info!("Starting Bluetooth monitoring...");

        // Create Bluetooth monitor
        info!("Creating Bluetooth monitor...");
        let monitor = create_btleplug_monitor()
            .await
            .map_err(AppError::Bluetooth)?;
        info!("Bluetooth monitor created");

        // Check if adapter is powered
        let powered = monitor
            .is_powered()
            .await
            .map_err(AppError::Bluetooth)?;
        if !powered {
            info!("Bluetooth adapter is not powered on. Device discovery may be limited.");
        } else {
            info!("Bluetooth adapter is powered on");

            // Get adapter info
            if let Ok(info_str) = monitor.adapter_info().await {
                info!("Adapter: {}", info_str);
            }
        }

        // Run the FullNode event loop
        // This will monitor Bluetooth devices, sign occurrences, and store them
        if let Err(e) = self.node.run(monitor).await {
            error!("FullNode error: {}", e);
            return Err(AppError::FullNode(e.to_string()));
        }

        info!("Application completed");
        Ok(())
    }

    /// Get a reference to the underlying FullNode.
    ///
    /// This can be used for testing or direct access to node functionality.
    pub fn node(&self) -> &FullNode {
        &self.node
    }

    /// Get the node ID.
    pub fn node_id(&self) -> &[u8] {
        self.node.node_id()
    }

    /// Get the data directory.
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    /// Print current statistics.
    pub fn print_stats(&self) {
        let stats = self.node.stats();
        info!("=== FullNode Statistics ===");
        info!("{:?}", stats);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_app_creation_without_db() {
        // This test verifies that app creation fails gracefully without a database
        let config = Config {
            database_url: None,
            pg_host: Some("localhost".to_string()),
            pg_port: Some(5432),
            pg_database: None, // Missing
            pg_user: Some("testuser".to_string()),
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        let result = App::new(config).await;
        assert!(result.is_err());
    }
}
