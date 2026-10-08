//! Main application state and event loop.
//!
//! This module provides the `App` struct which integrates the FullNode
//! with the application lifecycle.

use log::{error, info};
use std::path::PathBuf;

use crate::config::AppConfig;
use crate::error::{AppError, Result};
use crate::node::full::{FullNode, FullNodeConfig};
use bt_mon::MonitorConfig;

/// Simulated beacons a `--use-mock-backend` run puts on the air.
#[cfg(feature = "mock")]
const MOCK_BEACONS: usize = 5;

/// Main application state.
///
/// This struct wraps the FullNode and manages the application lifecycle.
pub struct App {
    node: FullNode,
    data_dir: PathBuf,
    /// Which radio to open and how often to drive it. The monitor is built in
    /// [`App::run`], so these settings have to survive until then.
    monitor: MonitorConfig,
    #[cfg(feature = "mock")]
    use_mock_backend: bool,
}

impl App {
    /// Create a new application instance.
    ///
    /// This initializes the FullNode with the provided configuration,
    /// including database connection, node identity, rate limiting and the
    /// position source.
    ///
    /// Everything that can be checked without a network is checked first, so a
    /// misconfiguration fails in one message rather than after a connection
    /// timeout.
    pub async fn new(config: AppConfig) -> Result<Self> {
        // Validate configuration. Reported as a configuration problem rather than
        // the generic validation error, because that is what the operator has to
        // go and edit.
        config.validate_monitor().map_err(AppError::Config)?;

        // `--use-mock-backend` is accepted by the CLI whether or not the binary
        // was built with `mock`, and without this check it is then quietly
        // ignored: the operator asks for a simulation and their real adapter
        // gets used instead. Checked before the database connection so the
        // answer is one message rather than a D-Bus error three steps later.
        if config.bluetooth.use_mock_backend && !cfg!(feature = "mock") {
            return Err(AppError::Config(
                "the mock backend was requested, but this binary was built without the `mock` \
                 feature; run `cargo build --features mock` (or `cargo run --features mock`)"
                    .to_string(),
            ));
        }

        // Build database URL from config (either DSN or components)
        let database_url = config.database_url().map_err(AppError::Config)?;

        // The position chain is assembled here, so a location the node cannot
        // ever acquire (`mode = "fixed"` with no fixed point) stops startup rather
        // than quietly storing NULL locations for the lifetime of the process.
        // Nothing is opened: a gpsd source connects on first use.
        let position = config
            .position_source()
            .map_err(|e| AppError::Config(e.to_string()))?;

        info!("Connecting to database...");
        let pool = repo::Pool::connect(&database_url)
            .await
            .map_err(AppError::Database)?;
        info!("Connected to database");

        // Get data directory
        let data_dir = config.data_dir().map_err(AppError::Config)?;
        info!("Data directory: {:?}", data_dir);

        // Create data directory if it doesn't exist
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| AppError::Io(format!("Failed to create data directory: {}", e)))?;

        // Build FullNode configuration
        let fullnode_config = FullNodeConfig {
            pool,
            data_dir: data_dir.clone(),
            rate_limit_threshold_ms: config.bluetooth.rate_limit_ms,
            rate_limit_max_cache_size: None, // Could be configurable
            position,
            stream_reopen_delay_ms: config.bluetooth.stream_reopen_delay_ms,
            store_raw_payload: config.bluetooth.store_raw_payload,
            // A stated node id is an assertion about the key in data_dir, checked
            // by FullNode::new against the identity it loads. Already validated
            // above, so this only parses what validate_monitor accepted.
            expected_node_id: config.expected_node_id().map_err(AppError::Config)?,
            // Presence is trusted for a multiple of the sampling window rather
            // than set here: the window is the operator's knob, and how long a
            // device is worth re-observing is a function of it. See
            // `FullNodeConfig::presence_timeout_ms`.
            presence_timeout_ms: None,
            clock: None,
            #[cfg(feature = "mock")]
            use_mock_backend: config.bluetooth.use_mock_backend,
            // `[revocation]` as resolved from flag/file/env. Enabled means the node
            // must reach a current, verified list from its CA: FullNode::new fails
            // startup rather than run a node that cannot attest to what it stores.
            revocation: config.revocation.clone(),
        };

        // Which radio to open, and how often to re-arm the scan once it is open.
        // Resolved at startup rather than at first scan so a selector naming an
        // adapter this machine does not have fails here, where the operator is
        // still watching, instead of on the first device event.
        let monitor = config.bluetooth.monitor_config();

        // Create FullNode instance
        info!("Initializing FullNode...");
        let node = FullNode::new(fullnode_config).await?;
        info!(
            "FullNode initialized with node ID: {}",
            hex::encode(node.node_id())
        );

        Ok(Self {
            node,
            data_dir,
            monitor,
            #[cfg(feature = "mock")]
            use_mock_backend: config.bluetooth.use_mock_backend,
        })
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
        info!(
            "Scan interval: {} ms (the scan is continuous and is re-armed on this cadence)",
            self.monitor.scan_interval.as_millis()
        );
        match self.monitor.adapter.as_deref() {
            Some(selector) => info!("Adapter selector: '{selector}'"),
            None => info!("No adapter selected; using the first one the system reports"),
        }

        #[cfg(feature = "mock")]
        let mut monitor: Box<dyn bt_mon::DeviceMonitor + Send + Sync> = if self.use_mock_backend {
            info!("Using mock backend (no physical Bluetooth required)");
            // Advertise a few beacons rather than an empty room. A monitor with
            // nothing in range is the right harness for a unit test and the wrong
            // thing to start a node against: the scan succeeds, every stream
            // stays open, and not one occurrence is ever stored, which looks
            // exactly like a node that is working.
            let config = bt_mon::backends::mock::MockConfig::default()
                .with_advertiser_count(MOCK_BEACONS)
                // The simulated radio obeys the same setting as a real one: this
                // is how often the node hears from it.
                .with_advertise_interval_ms(
                    u64::try_from(self.monitor.scan_interval.as_millis()).unwrap_or(u64::MAX),
                );
            // The mock reports the adapter it was told to be, so `--adapter-id`
            // is observable end to end rather than accepted and dropped.
            let config = match self.monitor.adapter.as_deref() {
                Some(selector) => config.with_adapter_name(selector),
                None => config,
            };
            Box::new(
                bt_mon::create_mock_monitor_with_config(config)
                    .await
                    .map_err(AppError::Bluetooth)?,
            )
        } else {
            info!("Using btleplug backend");
            Box::new(
                bt_mon::create_btleplug_monitor_with_config(self.monitor.clone())
                    .await
                    .map_err(AppError::Bluetooth)?,
            )
        };

        #[cfg(not(feature = "mock"))]
        let mut monitor: Box<dyn bt_mon::DeviceMonitor + Send + Sync> = {
            info!("Using btleplug backend");
            Box::new(
                bt_mon::create_btleplug_monitor_with_config(self.monitor.clone())
                    .await
                    .map_err(AppError::Bluetooth)?,
            )
        };

        info!("Bluetooth monitor created");

        // Check if adapter is powered
        let powered = monitor
            .as_ref()
            .is_powered()
            .await
            .map_err(AppError::Bluetooth)?;
        if !powered {
            info!("Bluetooth adapter is not powered on. Device discovery may be limited.");
        } else {
            info!("Bluetooth adapter is powered on");

            // Get adapter info
            if let Ok(info_str) = monitor.as_ref().adapter_info().await {
                info!("Adapter: {}", info_str);
            }
        }

        // Run the FullNode event loop
        // This will monitor Bluetooth devices, sign occurrences, and store them
        if let Err(e) = self.node.run(&mut *monitor).await {
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
    use crate::config::env_file::EnvLayer;
    use crate::config::file::ConfigFile;
    use crate::config::layered::resolve;
    use crate::config::{Flags, DEFAULT_SCAN_INTERVAL_MS};
    use std::collections::HashMap;
    use std::time::Duration;

    /// Resolve a configuration the way the binary does, but insulated from the
    /// environment of whoever runs the test.
    fn settings(contents: &str) -> AppConfig {
        resolve(
            &Flags::default(),
            &ConfigFile::parse(contents).expect("fixture should parse"),
            &EnvLayer::from_map(HashMap::new()),
        )
        .expect("fixture should resolve")
        .config
    }

    const COMPLETE: &str = r#"
[database]
url = "postgres://test:test@127.0.0.1:9/travel"
"#;

    /// A node id as the node itself prints it: 32 bytes of SHA-256 over the
    /// signing key, in hex.
    const NODE_ID: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

    #[tokio::test]
    async fn startup_reports_a_missing_database_before_trying_to_connect() {
        // No DSN and no database name: the failure must come from validation, not
        // from a connection attempt that would take seconds and say less.
        let config = settings("[database]\nuser = \"testuser\"\n");

        let Err(error) = App::new(config).await else {
            panic!("expected startup to fail without a database");
        };
        assert!(
            matches!(error, AppError::Config(_)),
            "expected a configuration error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn startup_rejects_a_location_the_node_could_never_acquire() {
        let config = settings(&format!(
            "{COMPLETE}[node]\nid = \"{NODE_ID}\"\n[location]\nmode = \"fixed\"\n"
        ));

        let Err(error) = App::new(config).await else {
            panic!("expected startup to fail with an unbuildable location");
        };
        let AppError::Config(message) = error else {
            panic!("expected a configuration error, got {error:?}");
        };
        assert!(message.contains("fixed"), "got: {message}");
    }

    #[tokio::test]
    async fn startup_refuses_a_node_id_from_an_older_setup_before_connecting() {
        // `NODE_ID` used to be asked for as a UUID. A node left with that value
        // has to be told what a node id is now, and told before a database
        // connection is opened on the strength of it.
        let config = settings(&format!(
            "{COMPLETE}[node]\nid = \"550e8400-e29b-41d4-a716-446655440000\"\n"
        ));

        let Err(error) = App::new(config).await else {
            panic!("expected startup to fail on a node id that cannot hold 32 bytes");
        };
        let AppError::Config(message) = error else {
            panic!("expected a configuration error, got {error:?}");
        };
        assert!(message.contains("UUID"), "got: {message}");
        assert!(message.contains("64 hex"), "got: {message}");
    }

    #[test]
    fn the_radio_settings_reach_the_monitor_constructor() {
        // The two settings that decide which adapter is opened and how the scan is
        // driven are constructor arguments, so the handover is where they can be
        // lost — and where losing them is visible.
        let config = settings(
            "[bluetooth]\nadapter_id = \"hci1\"\nscan_interval_ms = 250\nstore_raw_payload = false\n",
        );

        let monitor = config.bluetooth.monitor_config();

        assert_eq!(monitor.adapter.as_deref(), Some("hci1"));
        assert_eq!(monitor.scan_interval, Duration::from_millis(250));
        assert!(!config.bluetooth.store_raw_payload);
    }

    #[test]
    fn an_unstated_adapter_leaves_the_choice_to_the_monitor() {
        let config = settings("");

        let monitor = config.bluetooth.monitor_config();

        assert_eq!(monitor.adapter, None);
        assert_eq!(
            monitor.scan_interval,
            Duration::from_millis(DEFAULT_SCAN_INTERVAL_MS)
        );
    }
}
