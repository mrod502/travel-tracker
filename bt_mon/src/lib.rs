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
//! # Use both backends
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
pub mod error;
pub mod types;

// Re-export core types for convenience
pub use error::{BackendKind, Error, Result};
pub use types::{
    BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId, GattCharacteristic,
    GattService, ServiceUuid, UpdateField, ValueNotification,
};

// Monitor traits
pub mod monitor;

// Re-export traits for convenience
pub use monitor::{DeviceMonitor, GattClient};

// Re-export event types
pub use monitor::events::{DeviceEvent, NotificationEvent};

// Backend implementations
#[cfg(any(feature = "btleplug", feature = "bluer"))]
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

#[cfg(test)]
mod tests {
    #[test]
    fn test_library_compiles() {
        // This test just verifies that the library compiles with default features
        assert!(true);
    }
}
