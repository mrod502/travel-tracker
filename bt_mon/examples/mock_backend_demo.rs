//! Example demonstrating the mock backend for testing and development.
//!
//! This example shows how to use the mock backend to simulate Bluetooth
//! devices and operations without requiring physical hardware.
//!
//! Run with:
//! ```bash
//! cargo run --example mock_backend_demo --features mock
//! ```

use bt_mon::{
    backends::mock::{MockConfig, MockMonitor, SimulatedCharacteristic, SimulatedService},
    CharacteristicProperties, CharacteristicUuid, DeviceId, DeviceMonitor, GattClient,
    ServiceUuid,
};
use log::info;

#[tokio::main]
async fn main() -> Result<(), bt_mon::Error> {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    info!("=== Mock Backend Demo ===\n");

    // Example 1: Basic mock monitor usage
    demo_basic_usage().await?;

    // Example 2: Configured mock monitor with custom settings
    demo_configured_monitor().await?;

    // Example 3: Simulating GATT services and characteristics
    demo_gatt_simulation().await?;

    // Example 4: Error handling simulation
    demo_error_simulation().await?;

    info!("=== All demos completed successfully! ===");
    Ok(())
}

/// Demonstrate basic mock monitor usage
async fn demo_basic_usage() -> Result<(), bt_mon::Error> {
    info!("\n--- Demo 1: Basic Usage ---");

    // Create a mock monitor with default configuration
    let monitor = MockMonitor::new();

    // Add simulated devices
    let device_ids = vec![
        DeviceId::new("00:11:22:33:44:55"),
        DeviceId::new("AA:BB:CC:DD:EE:FF"),
        DeviceId::new("12:34:56:78:9A:BC"),
    ];

    for id in &device_ids {
        monitor.add_device(id.clone()).await?;
        info!("Added device: {}", id);
    }

    // Start scanning (simulated)
    monitor.start_scan().await?;
    info!("Scan started (simulated)");

    // Get discovered devices
    let devices = monitor.devices().await?;
    info!("Discovered {} devices", devices.len());

    for device in &devices {
        info!(
            "  - {} (name: {:?}, rssi: {:?})",
            device.id, device.name, device.rssi
        );
    }

    // Stop scanning
    monitor.stop_scan().await?;
    info!("Scan stopped");

    // Connect to a device
    monitor.connect(&device_ids[0]).await?;
    info!("Connected to device: {}", device_ids[0]);

    // Check connection status
    let is_connected = monitor.is_connected(&device_ids[0]).await?;
    info!("Device {} is connected: {}", device_ids[0], is_connected);

    // Disconnect
    monitor.disconnect(&device_ids[0]).await?;
    info!("Disconnected from device: {}", device_ids[0]);

    // Clean up
    monitor.clear_devices().await;
    info!("Cleared all devices");

    Ok(())
}

/// Demonstrate configured mock monitor with custom settings
async fn demo_configured_monitor() -> Result<(), bt_mon::Error> {
    info!("\n--- Demo 2: Configured Monitor ---");

    // Create a mock monitor with custom configuration
    let config = MockConfig::default()
        .with_adapter_name("CustomMockAdapter")
        .with_default_rssi(-75)
        .with_scan_delay_ms(50)
        .with_read_delay_ms(25)
        .with_write_delay_ms(25)
        .with_connect_delay_ms(100)
        .with_adapter_powered(true);

    let monitor = MockMonitor::with_config(config);

    // Check adapter info
    let adapter_info = monitor.adapter_info().await?;
    info!("Adapter info: {}", adapter_info);

    // Check if powered
    let is_powered = monitor.is_powered().await?;
    info!("Adapter powered: {}", is_powered);

    // Add a device
    let device_id = DeviceId::new("FF:EE:DD:CC:BB:AA");
    monitor.add_device(device_id.clone()).await?;
    info!("Added device with custom RSSI: {}", device_id);

    // Verify device properties
    let device = monitor.device(&device_id).await?;
    info!("Device RSSI: {:?} (should be -75)", device.rssi);

    Ok(())
}

/// Demonstrate simulating GATT services and characteristics
async fn demo_gatt_simulation() -> Result<(), bt_mon::Error> {
    info!("\n--- Demo 3: GATT Simulation ---");

    let monitor = MockMonitor::new();

    // Define a simulated service (e.g., Device Information Service)
    let device_info_service = SimulatedService::new(
        ServiceUuid::parse_str("0000180a-0000-1000-8000-00805f9b34fb").unwrap(),
        true,
    )
    .with_characteristic(SimulatedCharacteristic::new(
        CharacteristicUuid::parse_str("00002a29-0000-1000-8000-00805f9b34fb").unwrap(),
        CharacteristicProperties::new().with_read(true),
    ))
    .with_characteristic(SimulatedCharacteristic::new(
        CharacteristicUuid::parse_str("00002a24-0000-1000-8000-00805f9b34fb").unwrap(),
        CharacteristicProperties::new().with_read(true),
    ));

    // Define a simulated service (e.g., Battery Service)
    let battery_service = SimulatedService::new(
        ServiceUuid::parse_str("0000180f-0000-1000-8000-00805f9b34fb").unwrap(),
        true,
    )
    .with_characteristic(SimulatedCharacteristic::new(
        CharacteristicUuid::parse_str("00002a19-0000-1000-8000-00805f9b34fb").unwrap(),
        CharacteristicProperties::new().with_read(true).with_notify(true),
    ));

    // Add a device with custom services
    let device_id = DeviceId::new("11:22:33:44:55:66");
    monitor
        .add_device_with_services(
            device_id.clone(),
            "Smart Watch",
            vec![device_info_service, battery_service],
        )
        .await?;

    info!("Added device 'Smart Watch' with custom GATT services");

    // Start scanning and connect
    monitor.start_scan().await?;
    monitor.connect(&device_id).await?;
    info!("Connected to Smart Watch");

    // Discover services
    let services = monitor.discover_services(&device_id).await?;
    info!("Discovered {} services:", services.len());

    for service in &services {
        info!(
            "  - Service: {} (primary: {})",
            service.uuid, service.is_primary
        );
        for char in &service.characteristics {
            info!(
                "    - Characteristic: {} (read: {}, notify: {})",
                char.uuid,
                char.can_read(),
                char.can_notify()
            );
        }
    }

    // Read a characteristic (will return empty Vec for unconfigured values)
    let device_info_char =
        CharacteristicUuid::parse_str("00002a29-0000-1000-8000-00805f9b34fb").unwrap();

    let value = monitor.read_characteristic(&device_id, &device_info_char).await?;
    info!(
        "Read {} bytes from manufacturer name characteristic",
        value.len()
    );

    // Subscribe to notifications
    let battery_char =
        CharacteristicUuid::parse_str("00002a19-0000-1000-8000-00805f9b34fb").unwrap();

    monitor.subscribe(&device_id, &battery_char).await?;
    info!("Subscribed to battery level notifications");

    // Unsubscribe
    monitor.unsubscribe(&device_id, &battery_char).await?;
    info!("Unsubscribed from battery level notifications");

    Ok(())
}

/// Demonstrate error handling simulation
async fn demo_error_simulation() -> Result<(), bt_mon::Error> {
    info!("\n--- Demo 4: Error Simulation ---");

    // Create a mock monitor configured to simulate errors
    let config = MockConfig::default()
        .with_error_on_scan(true)
        .with_scan_error_message("Simulated adapter error")
        .with_error_on_connect(true)
        .with_connect_error_message("Simulated connection timeout")
        .with_error_on_read(true)
        .with_read_error_message("Simulated GATT read error")
        .with_error_on_write(true)
        .with_write_error_message("Simulated GATT write error");

    let monitor = MockMonitor::with_config(config);

    // Add a device
    let device_id = DeviceId::new("AB:CD:EF:12:34:56");
    monitor.add_device(device_id.clone()).await?;

    // Test scan error
    let scan_result = monitor.start_scan().await;
    match scan_result {
        Ok(_) => info!("ERROR: Scan should have failed!"),
        Err(e) => info!("✓ Scan error simulated: {}", e),
    }

    // Create a new monitor without scan error for connection test
    let config2 = MockConfig::default()
        .with_error_on_connect(true)
        .with_connect_error_message("Simulated pairing failure");
    let monitor2 = MockMonitor::with_config(config2);

    monitor2.add_device(device_id.clone()).await?;

    // Test connection error
    let connect_result = monitor2.connect(&device_id).await;
    match connect_result {
        Ok(_) => info!("ERROR: Connection should have failed!"),
        Err(e) => info!("✓ Connection error simulated: {}", e),
    }

    // Create a monitor for read error testing (scan and connect work)
    let config3 = MockConfig::default()
        .with_error_on_read(true)
        .with_read_error_message("Authorization denied");
    let monitor3 = MockMonitor::with_config(config3);

    monitor3.add_device(device_id.clone()).await?;
    monitor3.connect(&device_id).await?;
    info!("Connected successfully");

    // Test read error
    let test_char = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
    let read_result = monitor3.read_characteristic(&device_id, &test_char).await;
    match read_result {
        Ok(_) => info!("ERROR: Read should have failed!"),
        Err(e) => info!("✓ Read error simulated: {}", e),
    }

    // Create a monitor for write error testing
    let config4 = MockConfig::default()
        .with_error_on_write(true)
        .with_write_error_message("Insufficient authentication");
    let monitor4 = MockMonitor::with_config(config4);

    monitor4.add_device(device_id.clone()).await?;
    monitor4.connect(&device_id).await?;

    // Test write error
    let write_result =
        monitor4.write_characteristic(&device_id, &test_char, &[0x01, 0x02], true).await;
    match write_result {
        Ok(_) => info!("ERROR: Write should have failed!"),
        Err(e) => info!("✓ Write error simulated: {}", e),
    }

    info!("All error scenarios tested successfully");

    Ok(())
}
