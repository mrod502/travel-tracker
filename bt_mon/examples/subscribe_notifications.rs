//! Example: Subscribe to Notifications
//!
//! This example demonstrates how to subscribe to characteristic
//! notifications and receive value updates.
//!
//! # Running the example
//!
//! ```bash
//! # Using the default (btleplug) backend
//! cargo run --example subscribe_notifications -- <device_id> <characteristic_uuid>
//!
//! # Using the bluer backend (Linux only)
//! cargo run --example subscribe_notifications --features bluer --no-default-features -- <device_id> <characteristic_uuid>
//! ```
//!
//! # Arguments
//!
//! - `device_id`: The ID of the device to connect to
//! - `characteristic_uuid`: The UUID of the characteristic to subscribe to
//!
//! # Requirements
//!
//! - Bluetooth adapter must be powered on
//! - Appropriate permissions to access Bluetooth hardware
//! - A connected BLE device that supports notifications
//!
//! # Example
//!
//! ```bash
//! # Subscribe to Heart Rate Measurement (0x2A37)
//! cargo run --example subscribe_notifications -- AA:BB:CC:DD:EE:FF 00002a37-0000-1000-8000-00805f9b34fb
//! ```

use bt_mon::{DeviceMonitor, GattClient, CharacteristicUuid, DeviceId, create_btleplug_monitor};
use futures_util::stream::StreamExt;
use log::{info, warn, debug};
use std::env;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .init();

    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <device_id> <characteristic_uuid>", args[0]);
        eprintln!();
        eprintln!("Example:");
        eprintln!("  {} AA:BB:CC:DD:EE:FF 00002a37-0000-1000-8000-00805f9b34fb", args[0]);
        eprintln!();
        eprintln!("Common characteristic UUIDs:");
        eprintln!("  - Device Name: 00002a00-0000-1000-8000-00805f9b34fb");
        eprintln!("  - Heart Rate Measurement: 00002a37-0000-1000-8000-00805f9b34fb");
        eprintln!("  - Battery Level: 00002a19-0000-1000-8000-00805f9b34fb");
        return Err("Arguments required".into());
    }
    
    let device_id = DeviceId::new(&args[1]);
    let char_uuid = CharacteristicUuid::parse_str(&args[2])?;
    
    info!("Creating Bluetooth monitor...");
    let monitor = create_btleplug_monitor().await?;
    
    // Check if adapter is powered
    let powered = monitor.is_powered().await?;
    if !powered {
        warn!("Bluetooth adapter is not powered on.");
        return Err("Bluetooth adapter not powered".into());
    }
    info!("Bluetooth adapter is powered on");
    
    // Try to find and connect to the device
    info!("Connecting to device {}...", device_id);
    
    // Start a quick scan to find the device
    monitor.start_scan().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    monitor.stop_scan().await?;
    
    monitor.connect(&device_id).await?;
    
    // Wait for connection to stabilize
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // Check connection status
    let connected = monitor.is_connected(&device_id).await?;
    if !connected {
        warn!("Failed to connect to device");
        return Err("Connection failed".into());
    }
    info!("Connected to device");
    
    // Discover services to ensure the characteristic is available
    info!("Discovering services...");
    let _services = monitor.discover_services(&device_id).await?;
    info!("Services discovered");
    
    // Subscribe to notifications
    info!("Subscribing to characteristic {}...", char_uuid);
    monitor.subscribe(&device_id, &char_uuid).await?;
    info!("Subscribed to notifications");
    
    // Listen for notifications
    info!("Waiting for notifications (press Ctrl+C to stop)...");
    let mut notifications = monitor.notifications(&device_id).await?;
    
    while let Some(notification) = notifications.next().await {
        info!(
            "Notification from {}: {:02x?}",
            notification.characteristic,
            notification.value
        );
        
        // Try to decode as UTF-8 string
        if let Ok(text) = std::str::from_utf8(&notification.value) {
            debug!("  As string: '{}'", text);
        }
        
        // For heart rate monitors, decode the value
        if notification.characteristic == CharacteristicUuid::parse_str("00002a37-0000-1000-8000-00805f9b34fb")? {
            if notification.value.len() >= 2 {
                let flags = notification.value[0];
                let heart_rate_is_16bit = (flags & 0x01) == 0;
                
                if heart_rate_is_16bit {
                    if notification.value.len() >= 4 {
                        let hr = u16::from_le_bytes([notification.value[1], notification.value[2]]);
                        info!("  Heart Rate: {} bpm", hr);
                    }
                } else {
                    let hr = notification.value[1];
                    info!("  Heart Rate: {} bpm", hr);
                }
            }
        }
    }
    
    // Unsubscribe on exit
    info!("Unsubscribing...");
    monitor.unsubscribe(&device_id, &char_uuid).await?;
    
    // Disconnect
    info!("Disconnecting...");
    monitor.disconnect(&device_id).await?;
    info!("Done");
    
    Ok(())
}
