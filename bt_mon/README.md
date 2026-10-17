# bt_mon

**Bluetooth device monitoring library with backend abstraction.**

A Rust library for discovering and interacting with Bluetooth Low Energy (BLE) devices. It provides a unified interface that runs on macOS, Windows and Linux through `btleplug`, on Linux/BlueZ through `bluer`, or against a simulated radio through `mock`.

## Features

- **Backend abstraction**: `btleplug` (cross-platform), `bluer` (Linux/BlueZ), or `mock` (no hardware)
- **Device discovery**: Scan for nearby Bluetooth devices, with the scan re-armed on a cadence so a session that lapsed cannot leave a running node deaf
- **GATT client operations**: Read, write (with or without an att response), and subscribe to notifications and indications
- **Device events**: `DeviceAdded`, `DeviceUpdated` (naming which fields moved), `Advertisement`, `DeviceRemoved`
- **Monitor health**: whether the radio is actually able to deliver reports — a powered-off adapter or a scan that will not re-arm is otherwise indistinguishable from an empty room
- **Async-friendly**: Built on tokio runtime
- **Type-safe API**: Compile-time safety with UUID wrappers and strong typing

This crate is a **central** only: it scans and talks to peripherals. It exposes no GATT server and no BLE advertiser on any backend.

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

    // Open the event stream before scanning: a stream opened later only sees the
    // reports that arrive after it.
    let mut events = monitor.device_events().await?;

    // Start scanning for devices
    monitor.start_scan().await?;
    // ...
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

### Driving off the event stream

`devices()` is a snapshot; `device_events()` is what the radio said and which fields of it
moved. A stationary beacon keeps producing `Advertisement` — nothing is swallowed, because
"no report" is indistinguishable from "the device left". Whether a sighting deserves a row
is the consumer's policy.

```rust
use bt_mon::{DeviceMonitor, create_btleplug_monitor};
use futures::stream::StreamExt;

#[tokio::main]
async fn main() -> Result<(), bt_mon::Error> {
    let monitor = create_btleplug_monitor().await?;
    let mut events = monitor.device_events().await?;
    monitor.start_scan().await?;

    while let Some(event) = events.next().await {
        match event {
            bt_mon::DeviceEvent::DeviceAdded { device } => println!("new: {}", device.id),
            bt_mon::DeviceEvent::DeviceUpdated { device, changed_fields } => {
                println!("changed: {} {:?}", device.id, changed_fields)
            }
            bt_mon::DeviceEvent::Advertisement { device, .. } => {
                println!("still here: {} rssi={:?}", device.id, device.rssi)
            }
            bt_mon::DeviceEvent::DeviceRemoved { device_id } => println!("gone: {}", device_id),
        }
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

    // Connect, discover services, then subscribe. btleplug looks a characteristic up
    // in the table `discover_services` fills, so skipping it gives CharacteristicNotFound.
    monitor.connect(&device_id).await?;
    monitor.discover_services(&device_id).await?;
    monitor.subscribe(&device_id, &char_uuid).await?;

    // One stream per device, carrying every frame that device sends — including frames
    // another consumer subscribed for. Each frame names the characteristic it arrived on
    // and the service that owns it.
    let mut notifications = monitor.notifications(&device_id).await?;

    while let Some(notification) = notifications.next().await {
        println!("{} {:02x?}", notification.characteristic, notification.value);
    }

    Ok(())
}
```

`unsubscribe` stops that characteristic's notifications; on bluer it ends the BlueZ session,
which is what restores the CCCD on the device.

## Backends

### btleplug (Default, Cross-Platform)

The `btleplug` backend provides cross-platform support for macOS, Windows, and Linux.

```toml
[dependencies]
bt_mon = { version = "0.1.0", default-features = false, features = ["btleplug"] }
```

**Pros:**
- Works on macOS, Windows and Linux
- Simple API
- Actively maintained

**Cons:**
- Does not expose the advertisement bytes: `raw_payload` is always `None` here, and
  `raw_payload_source` is `None` with it — that means "this backend does not expose the
  bytes", never "the device advertised nothing"
- `services_resolved` is tracked by this crate (btleplug 0.12 has no such query), so it
  means "service discovery ran on this link", not a platform answer

### bluer (Linux Only)

The `bluer` backend talks to BlueZ directly over D-Bus. Requires BlueZ 5.43+.

```toml
[dependencies]
bt_mon = { version = "0.1.0", default-features = false, features = ["bluer"] }
tokio = { version = "1.35", features = ["full"] }
```

**Usage:**
```rust
use bt_mon::{DeviceMonitor, create_bluer_monitor};

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
- `services_resolved` is BlueZ's own property, not a proxy for the link
- `GattService::is_primary` is BlueZ's answer
- Discovery is armed with BlueZ's duplicate-data flag, so a beacon whose RSSI moved is
  reported again instead of being de-duplicated away
- A scan re-opens its discovery stream when the D-Bus subscription ends, so a
  `bluetoothd` restart does not silently end the node's hearing
- `raw_payload` carries the advertising structures, rebuilt from BlueZ's typed
  properties and labelled [`RawPayloadSource::Reconstructed`](bt_mon::RawPayloadSource)

**Cons:**
- Linux only, and needs BlueZ 5.43+ plus a reachable D-Bus
- BlueZ does not report the on-air order of the structures, so a reconstructed
  `raw_payload` is ordered by AD type; compare structures, not offsets, and never treat
  the absence of a type in it as evidence that the device did not advertise one

### Using Both Backends

You can enable both backends and choose at runtime:

```toml
[dependencies]
bt_mon = { version = "0.1.0", features = ["full"] }
```

### Mock (Testing/Development)

The `mock` backend provides a fully configurable simulated Bluetooth environment for testing and development without requiring physical hardware.

```toml
[dependencies]
bt_mon = { version = "0.1.0", default-features = false, features = ["mock"] }
tokio = { version = "1.35", features = ["full"] }
env_logger = "0.10"
```

**Usage:**
```rust
use bt_mon::{DeviceMonitor, GattClient, DeviceId, CharacteristicUuid};
use bt_mon::backends::mock::{MockConfig, MockMonitor};

#[tokio::main]
async fn main() -> Result<(), bt_mon::Error> {
    // Create a mock monitor with default configuration
    let monitor = MockMonitor::new();

    // Add simulated devices
    let device_id = DeviceId::new("AA:BB:CC:DD:EE:FF");
    monitor.add_device(device_id.clone()).await?;

    // Configure custom services
    // ... (see examples/mock_backend_demo.rs for details)

    // Start scanning (simulated)
    monitor.start_scan().await?;

    let devices = monitor.devices().await?;
    println!("Found {} simulated devices", devices.len());

    Ok(())
}
```

**Features:**

- **Simulated discovery**: Add/remove devices programmatically, or give the config
  advertisers so a development run sees a room with beacons in it
- **Simulated GATT services**: Define custom services and characteristics; `allow_read`,
  `allow_write` and `allow_subscribe` are enforced in addition to the declared
  `properties`, so "cannot be read" can be said once and means what it says
- **Injected notifications**: `send_notification` puts a frame on the device's stream, or
  `auto_send_notifications` replays configured values on a cadence after a `subscribe`
- **Error simulation**: Configure error responses for testing error handling
- **Configurable delays**: Simulate scan, connect, read and write latency
- **No hardware required**: Works in environments without Bluetooth interfaces

It reads `MockConfig`, not the `MonitorConfig` it is built from — `MockConfig::from`
translates an adapter selector into the adapter's reported name and a scan interval into
the advertisement cadence, so configuration stays observable through the mock too.

**Use Cases:**

- Unit testing without requiring physical hardware
- Development in headless environments
- Testing error handling scenarios
- Prototyping and demonstration

See `examples/mock_backend_demo.rs` for comprehensive examples.

## Examples

Each example is gated on the backend it drives, so the feature flags are part of the
command. Run one without its arguments and it prints what it wanted;
`bluer_scan_and_gatt` also takes `--help`.

```bash
# Scan with btleplug (the default feature)
cargo run --example device_discovery

# Connect and read a characteristic with btleplug
cargo run --example connect_and_read -- <device_id>

# Subscribe to notifications with btleplug
cargo run --example subscribe_notifications -- <device_id> <characteristic_uuid>

# Scan, then connect / discover / read / notify, on BlueZ
cargo run --example bluer_scan_and_gatt --features bluer --no-default-features -- \
    --adapter hci0 --scan 30
cargo run --example bluer_scan_and_gatt --features bluer --no-default-features -- \
    --device dev_F4_FE_FB_CC_78_58 --notify 00002a37-0000-1000-8000-00805f9b34fb

# The simulated radio: no hardware, no D-Bus
cargo run --example mock_backend_demo --features mock
```

`bluer_scan_and_gatt` is also the harness for the things only a radio can confirm — the
discovery re-arm, `services_resolved` after discovery, BlueZ's `is_primary`, and the
notification session behind `subscribe`. It prints the health counters at the end for
exactly that reason.

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

Every fallible call returns [`Error`](bt_mon::Error), and the variant says what to do about
it: retry the operation, re-open something, or tell the operator the radio is not there.

```rust
use bt_mon::{Error, DeviceId};

match monitor.connect(&device_id).await {
    Ok(()) => println!("Connected!"),
    Err(Error::DeviceNotFound(id)) => eprintln!("Never seen {} — scan first", id),
    Err(Error::NotConnected(id)) => eprintln!("{} is not connected", id),
    // The link existed and is gone now — the peripheral dropped out mid-operation.
    Err(Error::Disconnected(id)) => eprintln!("{} dropped out", id),
    // The operation reached the peripheral and it refused or failed; `operation` says
    // which one and `message` carries the stack's own words.
    Err(Error::GattError { device_id, operation, message }) => {
        eprintln!("{} on {}: {}", operation, device_id, message)
    }
    // A backend was asked for that this binary was not compiled with.
    Err(Error::BackendUnavailable { required, available }) => {
        eprintln!("{required} not built in; this binary has {available:?}")
    }
    Err(Error::BackendError { backend, message }) => {
        eprintln!("{}: {}", backend, message)
    }
    Err(e) => eprintln!("Unexpected error: {}", e),
}
```

The module documentation for [`Error`](bt_mon::Error) is the authority on which backend
raises which variant — several are raised by exactly one of the three.

## API Reference

Full API documentation is available via:

```bash
cargo doc --open
```

## Development

```bash
# Build
cargo build

# Tests (all three backends compiled in)
cargo test --all-features

# Format and lint
cargo fmt
cargo clippy --workspace --all-targets

# Generate documentation
cargo doc --open
```

Every backend module, entry point and example is feature-gated, so each combination has to
compile on its own — the crate's behaviour differs per backend and a cfg mistake is silent
until someone builds the combination that hits it:

```bash
for features in "" "btleplug" "bluer" "mock" "full"; do
  cargo check --no-default-features ${features:+--features $features}
done
```

`cargo clippy --workspace --all-targets` (not `-p bt_mon`) is the comparable warning count,
and it is what catches a cfg-gated example or test that nothing was compiling.

## License

MIT License - see [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Acknowledgments

- [btleplug](https://github.com/deviceplug/btleplug) - Cross-platform Bluetooth library
- [bluer](https://github.com/bluer-rs/bluer) - BlueZ binding for Rust
