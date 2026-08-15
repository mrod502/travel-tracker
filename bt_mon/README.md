# bt_mon

**Bluetooth device monitoring library with backend abstraction.**

A Rust library for discovering and interacting with Bluetooth Low Energy (BLE) devices. It provides a unified, backend-agnostic interface that works across macOS, Windows, and Linux.

## Features

- **Cross-platform support**: Works on macOS, Windows, and Linux
- **Backend abstraction**: Choose between `btleplug` (cross-platform) or `bluer` (Linux/BlueZ)
- **Device discovery**: Scan for nearby Bluetooth devices
- **GATT client operations**: Read, write, and subscribe to characteristics
- **Async-friendly**: Built on tokio runtime
- **Type-safe API**: Compile-time safety with UUID wrappers and strong typing

## Quick Start

Add `bt_mon` to your `Cargo.toml`:

```toml
[dependencies]
bt_mon = "0.1.0"
tokio = { version = "1.35", features = ["full"] }
```

## Basic Usage

### Device Discovery

```rust
use bt_mon::{DeviceMonitor, create_btleplug_monitor};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), bt_mon::Error> {
    // Create a monitor using the btleplug backend
    let monitor = create_btleplug_monitor().await?;
    
    // Check if Bluetooth is available
    let powered = monitor.is_powered().await?;
    if !powered {
        eprintln!("Bluetooth adapter is not powered on");
        return Err(bt_mon::Error::InitFailed("Bluetooth not powered".into()));
    }
    
    // Start scanning for devices
    monitor.start_scan().await?;
    
    // Wait for devices to be discovered
    tokio::time::sleep(Duration::from_secs(5)).await;
    
    // Stop scanning
    monitor.stop_scan().await?;
    
    // Get discovered devices
    let devices = monitor.devices().await?;
    println!("Found {} devices:", devices.len());
    
    for device in &devices {
        println!(
            "  - {} ({}): RSSI={:?}",
            device.name.as_deref().unwrap_or("Unknown"),
            device.address,
            device.rssi
        );
    }
    
    Ok(())
}
```

### Connect and Read

```rust
use bt_mon::{DeviceMonitor, GattClient, DeviceId, CharacteristicUuid};

#[tokio::main]
async fn main() -> Result<(), bt_mon::Error> {
    let monitor = create_btleplug_monitor().await?;
    
    // Connect to a device
    let device_id = DeviceId::new("AA:BB:CC:DD:EE:FF");
    monitor.connect(&device_id).await?;
    
    // Discover services
    monitor.discover_services(&device_id).await?;
    
    // Read the Device Name characteristic (0x2A00)
    let device_name_uuid = CharacteristicUuid::parse_str(
        "00002a00-0000-1000-8000-00805f9b34fb"
    )?;
    
    let value = monitor.read_characteristic(&device_id, &device_name_uuid).await?;
    let device_name = String::from_utf8_lossy(&value);
    println!("Device Name: {}", device_name);
    
    // Disconnect
    monitor.disconnect(&device_id).await?;
    
    Ok(())
}
```

### Subscribe to Notifications

```rust
use bt_mon::{DeviceMonitor, GattClient, DeviceId, CharacteristicUuid};
use futures::stream::StreamExt;

#[tokio::main]
async fn main() -> Result<(), bt_mon::Error> {
    let monitor = create_btleplug_monitor().await?;
    
    let device_id = DeviceId::new("AA:BB:CC:DD:EE:FF");
    let char_uuid = CharacteristicUuid::parse_str(
        "00002a37-0000-1000-8000-00805f9b34fb" // Heart Rate Measurement
    )?;
    
    // Connect and subscribe
    monitor.connect(&device_id).await?;
    monitor.discover_services(&device_id).await?;
    monitor.subscribe(&device_id, &char_uuid).await?;
    
    // Listen for notifications
    let mut notifications = monitor.notifications(&device_id).await?;
    
    while let Some(notification) = notifications.next().await {
        println!("Notification: {:02x?}", notification.value);
    }
    
    Ok(())
}
```

## Backends

### btleplug (Default, Cross-Platform)

The `btleplug` backend provides cross-platform support for macOS, Windows, and Linux.

```toml
[dependencies]
bt_mon = { version = "0.1.0", default-features = false, features = ["btleplug"] }
```

**Pros:**
- Works on macOS, Windows, and Linux
- Simple API
- Actively maintained

**Cons:**
- Limited feature set compared to platform-specific APIs
- No GATT server support

### bluer (Linux Only)

The `bluer` backend provides Linux-specific features using BlueZ. Requires BlueZ 5.43+.

```toml
[dependencies]
bt_mon = { version = "0.1.0", default-features = false, features = ["bluer"] }
tokio = { version = "1.35", features = ["full"] }
```

**Usage:**
```rust
use bt_mon::{DeviceMonitor, GattClient, create_bluer_monitor};

#[tokio::main]
async fn main() -> Result<(), bt_mon::Error> {
    // Create a monitor using the bluer backend (Linux only)
    let monitor = create_bluer_monitor().await?;
    
    // Use the same API as btleplug backend
    monitor.start_scan().await?;
    // ... rest of the code
    Ok(())
}
```

**Pros:**
- Full BlueZ feature set
- GATT server support
- BLE advertising support

**Cons:**
- Linux only
- Requires BlueZ 5.43+

### Using Both Backends

You can enable both backends and choose at runtime:

```toml
[dependencies]
bt_mon = { version = "0.1.0", features = ["full"] }
```

## Examples

Run the included examples:

```bash
# Device discovery
cargo run --example device_discovery

# Connect and read a characteristic
cargo run --example connect_and_read -- <device_id>

# Subscribe to notifications
cargo run --example subscribe_notifications -- <device_id> <characteristic_uuid>
```

## Common GATT UUIDs

### Standard Services

| UUID | Name |
|------|------|
| `00001800-0000-1000-8000-00805f9b34fb` | Generic Access (GAP) |
| `00001801-0000-1000-8000-00805f9b34fb` | Generic Attribute (GATT) |
| `0000180f-0000-1000-8000-00805f9b34fb` | Battery Service |
| `0000180d-0000-1000-8000-00805f9b34fb` | Heart Rate Service |

### Standard Characteristics

| UUID | Name |
|------|------|
| `00002a00-0000-1000-8000-00805f9b34fb` | Device Name |
| `00002a01-0000-1000-8000-00805f9b34fb` | Appearance |
| `00002a02-0000-1000-8000-00805f9b34fb` | Peripheral Privacy Flag |
| `00002a03-0000-1000-8000-00805f9b34fb` | Reconnection Address |
| `00002a04-0000-1000-8000-00805f9b34fb` | Peripheral Preferred Connection Parameters |
| `00002a19-0000-1000-8000-00805f9b34fb` | Battery Level |
| `00002a37-0000-1000-8000-00805f9b34fb` | Heart Rate Measurement |

## Platform Requirements

### macOS

- Bluetooth 4.0 (BLE) support
- No special permissions required for CLI apps

### Windows

- Bluetooth 4.0 (BLE) support
- Windows 10 or later recommended

### Linux

- Bluetooth 4.0 (BLE) support
- BlueZ 5.43 or later
- Root permissions may be required for some operations

## Error Handling

The library uses a custom `Error` type for all error conditions:

```rust
use bt_mon::{Error, DeviceId};

match monitor.connect(&device_id).await {
    Ok(()) => println!("Connected!"),
    Err(Error::DeviceNotFound(id)) => eprintln!("Device {} not found", id),
    Err(Error::NotConnected(id)) => eprintln!("Device {} not connected", id),
    Err(Error::BackendError { backend, message }) => {
        eprintln!("Backend error: {}", message)
    }
    Err(e) => eprintln!("Unexpected error: {}", e),
}
```

## API Reference

Full API documentation is available via:

```bash
cargo doc --open
```

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Format code
cargo fmt

# Lint code
cargo clippy

# Generate documentation
cargo doc --open
```

## License

MIT License - see [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Acknowledgments

- [btleplug](https://github.com/deviceplug/btleplug) - Cross-platform Bluetooth library
- [bluer](https://github.com/bluer-rs/bluer) - BlueZ binding for Rust
