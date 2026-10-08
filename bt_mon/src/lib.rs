//! bt_mon - Bluetooth device monitoring library with backend abstraction.
//!
//! This library provides a unified interface for discovering and interacting with
//! Bluetooth Low Energy (BLE) devices. It abstracts over the underlying backend,
//! supporting both `btleplug` (cross-platform) and `bluer` (Linux/BlueZ).
//!
//! # Features
//!
//! - Device discovery and scanning
//! - Device connection and disconnection
//! - GATT service and characteristic discovery
//! - Characteristic read/write operations
//! - Characteristic notification subscription
//!
//! # Backend Selection
//!
//! By default, bt_mon uses the `btleplug` backend (cross-platform). You can change this
//! by configuring features in your `Cargo.toml`:
//!
//! ```toml
//! # Use btleplug backend (default, cross-platform)
//! [dependencies]
//! bt_mon = { version = "0.1.0", default-features = false, features = ["btleplug"] }
//!
//! # Use bluer backend (Linux only, more features)
//! [dependencies]
//! bt_mon = { version = "0.1.0", default-features = false, features = ["bluer"] }
//!
//! # Use mock backend (testing/development)
//! [dependencies]
//! bt_mon = { version = "0.1.0", default-features = false, features = ["mock"] }
//!
//! # Use all backends including mock
//! [dependencies]
//! bt_mon = { version = "0.1.0", features = ["full"] }
//! ```
//!
//! # Basic Usage
//!
//! ```ignore
//! # #[tokio::main]
//! # async fn main() -> Result<(), bt_mon::Error> {
//! use bt_mon::{DeviceMonitor, create_btleplug_monitor};
//!
//! // Create a monitor using the btleplug backend (cross-platform)
//! let monitor = create_btleplug_monitor().await?;
//!
//! // Start scanning for devices
//! monitor.start_scan().await?;
//!
//! // Get discovered devices
//! let devices = monitor.devices().await?;
//! println!("Found {} devices", devices.len());
//!
//! # Ok(())
//! # }
//! ```

// Core modules
pub mod config;
pub mod error;
pub mod types;

// Re-export core types for convenience
pub use config::{adapter_matches, MonitorConfig, DEFAULT_SCAN_INTERVAL};
pub use error::{BackendKind, Error, Result};
pub use types::{
    BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId, GattCharacteristic,
    GattService, ServiceUuid, ValueNotification,
};

// Monitor traits
pub mod monitor;

// Re-export traits for convenience
pub use monitor::{DeviceMonitor, GattClient};

// Re-export event types
//
// `UpdateField` re-exports from the events module rather than `types`: it is a
// property of an event, and it used to live in `types` as a second, distinct
// enum of the same name.
pub use monitor::events::{
    changed_device_fields, report_event, DeviceEvent, NotificationEvent, UpdateField,
};

// Backend implementations
//
// `mock` belongs in this list: the crate's own documentation offers
// `default-features = false, features = ["mock"]` as a way to depend on the
// library without a Bluetooth stack, and gating the module on the real backends
// only makes that combination fail to compile.
#[cfg(any(feature = "btleplug", feature = "bluer", feature = "mock"))]
pub mod backends;

/// Create a new Bluetooth monitor using the btleplug backend (cross-platform).
///
/// This function is only available when the `btleplug` feature is enabled.
/// This is the default backend and provides cross-platform support for macOS,
/// Windows, and Linux.
///
/// # Errors
///
/// Returns an error if:
/// - No Bluetooth adapter is found on the system
/// - The adapter cannot be initialized
/// - Bluetooth is not available on the system
///
/// # Example
///
/// ```no_run
/// use bt_mon::{DeviceMonitor, create_btleplug_monitor};
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), bt_mon::Error> {
/// let monitor = create_btleplug_monitor().await?;
/// monitor.start_scan().await?;
/// let devices = monitor.devices().await?;
/// println!("Found {} devices", devices.len());
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "btleplug")]
pub async fn create_btleplug_monitor() -> Result<impl crate::monitor::GattClient> {
    crate::backends::btleplug::BtleplugMonitor::new().await
}

/// Create a btleplug monitor on the adapter and cadence `config` selects.
///
/// Use this whenever the operator has named an adapter or set a scan interval:
/// [`create_btleplug_monitor`] takes the first adapter the system reports and no
/// interval at all, so a setting that arrives here has to be handed over or it
/// stops existing at this boundary.
///
/// # Errors
///
/// As [`create_btleplug_monitor`], plus an error when `config` names an adapter
/// that this system does not have, or names one that more than one adapter
/// matches.
///
/// # Example
///
/// ```no_run
/// use bt_mon::{DeviceMonitor, MonitorConfig, create_btleplug_monitor_with_config};
/// use std::time::Duration;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), bt_mon::Error> {
/// let monitor = create_btleplug_monitor_with_config(
///     MonitorConfig::new().with_adapter("hci1").with_scan_interval(Duration::from_millis(500)),
/// )
/// .await?;
/// monitor.start_scan().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "btleplug")]
pub async fn create_btleplug_monitor_with_config(
    config: MonitorConfig,
) -> Result<crate::backends::btleplug::BtleplugMonitor> {
    crate::backends::btleplug::BtleplugMonitor::with_config(config).await
}

/// Create a new Bluetooth monitor using the bluer backend (Linux/BlueZ only).
///
/// This function is only available when the `bluer` feature is enabled.
/// The bluer backend provides Linux-specific features like GATT server support
/// and BLE advertising, but requires BlueZ 5.43+ and is Linux-only.
///
/// # Errors
///
/// Returns an error if:
/// - Not running on Linux
/// - No Bluetooth adapter is found
/// - The adapter cannot be initialized
/// - BlueZ is not available or too old
///
/// # Example
///
/// ```no_run
/// use bt_mon::{DeviceMonitor, create_bluer_monitor};
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), bt_mon::Error> {
/// let monitor = create_bluer_monitor().await?;
/// monitor.start_scan().await?;
/// let devices = monitor.devices().await?;
/// println!("Found {} devices", devices.len());
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "bluer")]
pub async fn create_bluer_monitor() -> Result<impl crate::monitor::GattClient> {
    crate::backends::bluer::BluerMonitor::new().await
}

/// Create a bluer monitor on the adapter and cadence `config` selects.
///
/// The bluer analogue of [`create_btleplug_monitor_with_config`]: BlueZ has a
/// default adapter, and "the default one" is not the same answer as "the one the
/// operator asked for".
///
/// # Errors
///
/// As [`create_bluer_monitor`], plus an error when `config` names an adapter
/// this session does not have, or one that several adapters match.
#[cfg(feature = "bluer")]
pub async fn create_bluer_monitor_with_config(
    config: MonitorConfig,
) -> Result<crate::backends::bluer::BluerMonitor> {
    crate::backends::bluer::BluerMonitor::with_config(config).await
}

/// Create a new Bluetooth monitor using the mock backend (testing/development).
///
/// This function is only available when the `mock` feature is enabled.
/// The mock backend provides a fully configurable simulated Bluetooth environment
/// for testing and development purposes, without requiring physical Bluetooth hardware.
///
/// # Features
///
/// - Simulated device discovery and connection
/// - Configurable error responses for testing error handling
/// - Simulated GATT services and characteristics
/// - Configurable delays to simulate network/Bluetooth latency
///
/// The return type is the concrete [`MockMonitor`](backends::mock::MockMonitor)
/// rather than `impl GattClient`: everything that makes the mock useful —
/// `add_device`, `remove_device`, `set_device_services` — is inherent to it, and
/// an opaque return type hides the API this function exists to hand out.
///
/// # Example
///
/// ```
/// use bt_mon::{DeviceMonitor, GattClient, create_mock_monitor, DeviceId};
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), bt_mon::Error> {
/// let monitor = create_mock_monitor().await?;
///
/// // Add simulated devices
/// let device_id = DeviceId::new("00:11:22:33:44:55");
/// monitor.add_device(device_id.clone()).await?;
///
/// // Start scanning (simulated)
/// monitor.start_scan().await?;
///
/// let devices = monitor.devices().await?;
/// println!("Found {} simulated devices", devices.len());
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "mock")]
pub async fn create_mock_monitor() -> Result<crate::backends::mock::MockMonitor> {
    Ok(crate::backends::mock::MockMonitor::new())
}

/// Create a mock monitor from a [`MockConfig`].
///
/// `create_mock_monitor` builds a harness: the room is empty until the caller
/// adds a device to it, so a scan of an unattended run discovers nothing. Put
/// advertisers in the config to get an environment instead — see
/// [`MockConfig::with_advertiser_count`].
///
/// [`MockConfig`]: backends::mock::MockConfig
///
/// # Example
///
/// ```
/// use bt_mon::{DeviceMonitor, create_mock_monitor_with_config};
/// use bt_mon::backends::mock::MockConfig;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), bt_mon::Error> {
/// let monitor = create_mock_monitor_with_config(
///     MockConfig::default().with_advertiser_count(3),
/// )
/// .await?;
///
/// monitor.start_scan().await?;
/// // The three beacons are now on the air, and any stream opened from here
/// // receives them.
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "mock")]
pub async fn create_mock_monitor_with_config(
    config: crate::backends::mock::MockConfig,
) -> Result<crate::backends::mock::MockMonitor> {
    Ok(crate::backends::mock::MockMonitor::with_config(config))
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_library_compiles() {
        // This test just verifies that the library compiles with default features
        assert!(true);
    }
}
