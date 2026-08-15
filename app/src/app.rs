//! Main application state and event loop.

use bt_mon::{create_btleplug_monitor, DeviceEvent, DeviceMonitor};
use bt_mon::monitor::events::UpdateField;
use futures_util::stream::StreamExt;
use log::{debug, error, info, warn};
use repo::{mac_address_from_string, Occurrence, OccurrenceRepository, Pool, SignalType};
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

        // Generate device hash (SHA-256 of MAC address for stable pseudonymous ID)
        let device_hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&device_address);
            hasher.finalize().to_vec()
        };

        // Build signal payload with Bluetooth-specific data
        let mut ble_payload = serde_json::Map::new();
        ble_payload.insert(
            "address_type".to_string(),
            serde_json::json!("public"), // Default, could be more specific
        );

        // Handle manufacturer data
        if let Some((company_id, payload)) = device.manufacturer_data.iter().next() {
            ble_payload.insert(
                "manufacturer_data".to_string(),
                serde_json::json!({
                    "company_id": company_id,
                    "payload": hex::encode(payload)
                }),
            );
        }

        // Handle service UUIDs
        if !device.service_data.is_empty() {
            let uuids: Vec<String> = device
                .service_data
                .keys()
                .map(|u| u.as_uuid().to_string())
                .collect();
            ble_payload.insert("service_uuids".to_string(), serde_json::json!(uuids));
        }

        // Add RSSI to payload if available
        if let Some(rssi) = device.rssi {
            ble_payload.insert("rssi".to_string(), serde_json::json!(rssi));
        }

        // Add device name if available
        if let Some(name) = &device.name {
            ble_payload.insert("name".to_string(), serde_json::json!(name));
        }

        // Wrap in signal_type key
        let mut signal_payload = serde_json::Map::new();
        signal_payload.insert("ble".to_string(), serde_json::json!(ble_payload));

        // Generate minimal signed payload (in production, this would be proper canonical CBOR)
        let signed_payload = format!(
            "{}:{}:{}",
            id,
            observed_at.timestamp(),
            hex::encode(&device_address)
        )
        .into_bytes();

        // Generate placeholder signature (in production, use real Ed25519 signing)
        let signature = vec![0u8; 64];

        // Build occurrence using builder pattern
        let occurrence = Occurrence::builder()
            .signal_type(SignalType::Bluetooth)
            .origin_node_id(&self.node_id.to_bytes_le().to_vec())
            .observed_at(observed_at)
            .observed_at_node_local(observed_at)
            .device_address(&device_address)
            .device_hash(&device_hash)
            .rssi(device.rssi.unwrap_or(0) as i16)
            .signal_payload(serde_json::Value::Object(signal_payload))
            .signed_payload(&signed_payload)
            .signature(&signature)
            .build();

        // Insert into database
        match OccurrenceRepository::create(self.pool.as_pool(), &occurrence).await {
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
