//! Example: Connect and Read
//!
//! This example demonstrates how to connect to a Bluetooth device
//! and read GATT characteristics.
//!
//! # Running the example
//!
//! ```bash
//! # Using the default (btleplug) backend
//! cargo run --example connect_and_read -- <device_id>
//!
//! # Using the bluer backend (Linux only)
//! cargo run --example connect_and_read --features bluer --no-default-features -- <device_id>
//! ```
//!
//! # Arguments
//!
//! - `device_id`: The ID of the device to connect to (e.g., MAC address)
//!
//! # Requirements
//!
//! - Bluetooth adapter must be powered on
//! - Appropriate permissions to access Bluetooth hardware
//! - A connected BLE device

use bt_mon::{DeviceMonitor, GattClient, CharacteristicUuid, DeviceId, create_btleplug_monitor};
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
    if args.len() < 2 {
        eprintln!("Usage: {} <device_id>", args[0]);
        eprintln!();
        eprintln!("Example device IDs:");
        eprintln!("  - MAC address: AA:BB:CC:DD:EE:FF");
        eprintln!("  - Platform-specific ID");
        return Err("Device ID required".into());
    }
    
    let device_id_str = &args[1];
    let device_id = DeviceId::new(device_id_str);
    
    info!("Creating Bluetooth monitor...");
    let monitor = create_btleplug_monitor().await?;
    
    // Check if adapter is powered
    let powered = monitor.is_powered().await?;
    if !powered {
        warn!("Bluetooth adapter is not powered on.");
        return Err("Bluetooth adapter not powered".into());
    }
    info!("Bluetooth adapter is powered on");
    
    // Try to find the device
    let devices = monitor.devices().await?;
    let device_found = devices.iter().any(|d| d.id == device_id);
    
    if !device_found {
        info!("Device not in cache. Starting scan to find it...");
        monitor.start_scan().await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        monitor.stop_scan().await?;
        
        let devices = monitor.devices().await?;
        if !devices.iter().any(|d| d.id == device_id) {
            warn!("Device {} not found. Make sure it's in range.", device_id);
            return Err("Device not found".into());
        }
    }
    
    info!("Connecting to device {}...", device_id);
    monitor.connect(&device_id).await?;
    
    // Wait a moment for connection to stabilize
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // Check connection status
    let connected = monitor.is_connected(&device_id).await?;
    if !connected {
        warn!("Failed to connect to device");
        return Err("Connection failed".into());
    }
    info!("Connected to device");
    
    // Discover services
    info!("Discovering services...");
    let services = monitor.discover_services(&device_id).await?;
    info!("Found {} services:", services.len());
    
    for service in &services {
        info!("  Service: {} (primary={})", service.uuid, service.is_primary);
        
        for char in &service.characteristics {
            info!("    Characteristic: {}", char.uuid);
            info!("      Properties:");
            if char.can_read() {
                info!("        - Read");
            }
            if char.can_write() {
                info!("        - Write");
            }
            if char.can_notify() {
                info!("        - Notify/Indicate");
            }
        }
    }
    
    // Try to read the Device Name characteristic (0x2A00)
    let device_name_uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb")?;
    
    if let Ok(value) = monitor.read_characteristic(&device_id, &device_name_uuid).await {
        if let Ok(text) = std::str::from_utf8(&value) {
            info!("Device Name: {}", text);
        } else {
            info!("Device Name (raw): {:02x?}", value);
        }
    } else {
        debug!("Could not read Device Name characteristic");
    }
    
    // Try to read the Appearance characteristic (0x2A01)
    let appearance_uuid = CharacteristicUuid::parse_str("00002a01-0000-1000-8000-00805f9b34fb")?;
    
    if let Ok(value) = monitor.read_characteristic(&device_id, &appearance_uuid).await {
        if value.len() >= 2 {
            let appearance = u16::from_le_bytes([value[0], value[1]]);
            info!("Appearance: 0x{:04x}", appearance);
        }
    } else {
        debug!("Could not read Appearance characteristic");
    }
    
    // Disconnect
    info!("Disconnecting...");
    monitor.disconnect(&device_id).await?;
    info!("Disconnected");
    
    Ok(())
}
