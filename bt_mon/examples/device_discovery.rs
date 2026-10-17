//! Example: Device Discovery
//!
//! This example demonstrates how to discover Bluetooth devices
//! using the bt_mon library.
//!
//! # Running the example
//!
//! ```bash
//! # btleplug backend (the default; this example is gated on that feature)
//! cargo run --example device_discovery
//! ```
//!
//! For the BlueZ backend see [`bluer_scan_and_gatt.rs`](bluer_scan_and_gatt.rs), which
//! also covers connect, service discovery, reads and notifications — the calls that need
//! a radio to verify and that this example does not exercise.
//!
//! # Requirements
//!
//! - Bluetooth adapter must be powered on
//! - Appropriate permissions to access Bluetooth hardware

use bt_mon::{create_btleplug_monitor, DeviceMonitor};
use log::{info, warn};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    info!("Creating Bluetooth monitor...");

    let monitor = create_btleplug_monitor().await?;

    // Check if adapter is powered
    let powered = monitor.is_powered().await?;
    if !powered {
        warn!("Bluetooth adapter is not powered on. Please power it on and try again.");
        return Err("Bluetooth adapter not powered".into());
    }
    info!("Bluetooth adapter is powered on");

    // Get adapter info
    let adapter_info = monitor.adapter_info().await?;
    info!("Adapter: {}", adapter_info);

    info!("Starting scan for 10 seconds...");
    monitor.start_scan().await?;

    // Wait for devices to be discovered
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Stop scanning
    monitor.stop_scan().await?;
    info!("Scan stopped");

    // Get discovered devices
    let devices = monitor.devices().await?;
    info!("Found {} devices:", devices.len());

    for device in &devices {
        info!(
            "  - {} ({}): RSSI={:?}",
            device.name.as_deref().unwrap_or("Unknown"),
            device.address,
            device.rssi
        );

        if !device.manufacturer_data.is_empty() {
            for (company_id, data) in &device.manufacturer_data {
                info!("    Manufacturer data (0x{:04}): {:02x?}", company_id, data);
            }
        }

        if !device.service_data.is_empty() {
            for (uuid, data) in &device.service_data {
                info!("    Service data ({}): {:02x?}", uuid, data);
            }
        }
    }

    Ok(())
}
