//! Main application state and event loop.

use bt_mon::{create_btleplug_monitor, DeviceEvent, DeviceMonitor};
use bt_mon::monitor::events::UpdateField;
use futures_util::stream::StreamExt;
use log::{debug, error, info, warn};
use repo::{mac_address_from_string, BluetoothOccurrence, BluetoothOccurrenceRepository, Pool};
use uuid::Uuid;

use crate::config::Config;
use crate::error::{AppError, Result};

/// Main application state.
pub struct App {
    config: Config,
    pool: Pool,
    node_id: Uuid,
}

impl App {
    /// Create a new application instance.
    pub async fn new(config: Config) -> Result<Self> {
        // Validate configuration
        config.validate()?;

        // Build database URL from config (either DSN or components)
        let database_url = config.database_url().map_err(AppError::from)?;

        // Connect to database
        info!("Connecting to database...");
        let pool = Pool::connect(&database_url)
            .await
            .map_err(AppError::Database)?;
        info!("Connected to database");

        // Parse node_id
        let node_id = config.node_id().map_err(AppError::from)?;
        info!("Node ID: {}", node_id);

        Ok(Self {
            config,
            pool,
            node_id,
        })
    }

    /// Run the main event loop.
    pub async fn run(&mut self) -> Result<()> {
        // Initialize logging
        env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or(&self.config.log_level)
        )
        .init();

        info!("Starting Bluetooth monitoring application");
        info!("Scan interval: {} ms", self.config.scan_interval_ms);

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
            warn!("Bluetooth adapter is not powered on. Some functionality may be limited.");
        } else {
            info!("Bluetooth adapter is powered on");

            // Get adapter info
            if let Ok(info_str) = monitor.adapter_info().await {
                info!("Adapter: {}", info_str);
            }
        }

        // Start scanning
        info!("Starting scan...");
        monitor
            .start_scan()
            .await
            .map_err(AppError::Bluetooth)?;
        info!("Scan started");

        // Get event stream
        let mut events = monitor
            .device_events()
            .await
            .map_err(AppError::Bluetooth)?;

        info!("Listening for Bluetooth events (press Ctrl+C to stop)...");

        // Main event loop
        while let Some(event) = events.next().await {
            match event {
                DeviceEvent::DeviceAdded { device } => {
                    info!("Discovered device: {}", device.id);
                    if let Err(e) = self.handle_device_added(&device).await {
                        error!("Error handling device added: {}", e);
                    }
                }

                DeviceEvent::DeviceRemoved { id } => {
                    debug!("Device removed: {}", id);
                    // For now, just log. We could track device presence.
                }

                DeviceEvent::DeviceUpdated {
                    device,
                    changed_fields,
                } => {
                    debug!(
                        "Device updated: {} (changed: {:?})",
                        device.id, changed_fields
                    );

                    // Handle RSSI updates - store as new occurrence
                    if changed_fields.contains(&UpdateField::Rssi) {
                        if let Err(e) = self.handle_device_updated(&device).await {
                            error!("Error handling device updated: {}", e);
                        }
                    }
                }
            }
        }

        info!("Event stream closed");

        // Stop scanning
        monitor
            .stop_scan()
            .await
            .map_err(AppError::Bluetooth)?;
        info!("Scan stopped");

        Ok(())
    }

    /// Handle a device added event.
    async fn handle_device_added(&self, device: &bt_mon::BluetoothDevice) -> Result<()> {
        // Parse MAC address
        let device_address = mac_address_from_string(device.id.as_str())
            .map_err(|e| AppError::InvalidMacAddress(e.to_string()))?;

        // Generate occurrence ID and timestamp
        let id = Uuid::now_v7();
        let observed_at = chrono::Utc::now();

        // Build occurrence using builder pattern
        let mut occurrence = BluetoothOccurrence::new(id, self.node_id, observed_at, &device_address)
            .with_node_local_timestamp(observed_at)
            .with_advertisement_type("ADV_IND"); // Default, could be more specific

        // Add optional fields
        if let Some(rssi) = device.rssi {
            occurrence = occurrence.with_rssi(rssi);
        }

        if let Some(name) = &device.name {
            occurrence = occurrence.with_advertised_name(name);
        }

        // Handle manufacturer data (take first entry if multiple)
        if let Some((company_id, payload)) = device.manufacturer_data.iter().next() {
            occurrence = occurrence.with_manufacturer_data(*company_id as i32, payload);
        }

        // Handle service UUIDs (extract keys from service_data map)
        if !device.service_data.is_empty() {
            let uuids: Vec<Vec<u8>> = device
                .service_data
                .keys()
                .map(|u| u.as_uuid().as_bytes().to_vec())
                .collect();
            occurrence = occurrence.with_service_uuids(uuids);
        }

        // Store raw payload if configured
        if self.config.store_raw_payload {
            // We don't have full raw payload in bt_mon currently, but we could add it
            // For now, skip this
        }

        // Insert into database
        match BluetoothOccurrenceRepository::create(self.pool.as_pool(), &occurrence).await {
            Ok(_) => {
                debug!("Stored occurrence for device: {}", device.id);
            }
            Err(e) => {
                // Don't fail the whole app on a single DB error
                error!("Failed to store occurrence: {}", e);
            }
        }

        Ok(())
    }

    /// Handle a device updated event.
    async fn handle_device_updated(&self, device: &bt_mon::BluetoothDevice) -> Result<()> {
        // For now, treat updates similar to additions
        // In the future, we might want different handling
        self.handle_device_added(device).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mac_address_parsing() {
        // Test valid MAC
        let mac = mac_address_from_string("AA:BB:CC:DD:EE:FF");
        assert!(mac.is_ok());
        assert_eq!(mac.unwrap(), vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);

        // Test invalid MAC
        let invalid = mac_address_from_string("AA:BB:CC:DD:EE");
        assert!(invalid.is_err());
    }
}
