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
/// This is the default backend.
///
/// # Example
///
/// ```ignore
/// use bt_mon::{DeviceMonitor, create_btleplug_monitor};
///
/// # async fn example() -> Result<(), bt_mon::Error> {
/// let monitor = create_btleplug_monitor().await?;
/// monitor.start_scan().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "btleplug")]
pub async fn create_btleplug_monitor() -> Result<impl crate::monitor::DeviceMonitor + crate::monitor::GattClient> {
    #[cfg(feature = "btleplug")]
    {
        crate::backends::BtleplugMonitor::new().await
    }
    #[cfg(not(feature = "btleplug"))]
    {
        Err(Error::BackendUnavailable {
            required: BackendKind::Btleplug,
            available: vec![],
        })
    }
}

/// Create a new Bluetooth monitor using the bluer backend (Linux/BlueZ).
///
/// This function is only available when the `bluer` feature is enabled.
/// The bluer backend provides additional features like GATT server support
/// and BLE advertising, but is Linux-only.
///
/// # Example
///
/// ```ignore
/// use bt_mon::{DeviceMonitor, create_bluer_monitor};
///
/// # async fn example() -> Result<(), bt_mon::Error> {
/// let monitor = create_bluer_monitor().await?;
/// monitor.start_scan().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "bluer")]
pub async fn create_bluer_monitor() -> Result<impl crate::monitor::DeviceMonitor + crate::monitor::GattClient> {
    #[cfg(feature = "bluer")]
    {
        crate::backends::BluerMonitor::new().await
    }
    #[cfg(not(feature = "bluer"))]
    {
        Err(Error::BackendUnavailable {
            required: BackendKind::Bluer,
            available: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_library_compiles() {
        // This test just verifies that the library compiles with default features
        assert!(true);
    }
}
