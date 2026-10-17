//! bt_mon - Bluetooth device monitoring library with backend abstraction.
//!
//! This library provides a unified interface for discovering and interacting with
//! Bluetooth Low Energy (BLE) devices. It abstracts over the underlying backend,
//! supporting `btleplug` (cross-platform), `bluer` (Linux/BlueZ), and a `mock`
//! simulated radio.
//!
//! # What this crate does
//!
//! - Scanning, with a scan that keeps itself armed rather than lapsing silently
//! - Device connection and disconnection
//! - GATT service and characteristic discovery, read and write
//! - Notification and indication subscription, as a per-device stream
//!   ([`GattClient::notifications`]) that carries every frame of that device
//!   regardless of who subscribed
//! - [`DeviceEvent`]s: what the radio said about a device, and which fields moved
//!   ([`changed_device_fields`])
//! - [`MonitorHealth`]: whether the monitor is able to hear the radio at all, which
//!   no device event can express — a dead adapter otherwise looks like an empty room
//!
//! This crate exposes no GATT *server* and no BLE advertiser. It is a central that
//! scans and talks to peripherals.
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
//! use futures::StreamExt;
//!
//! // Create a monitor using the btleplug backend (cross-platform)
//! let monitor = create_btleplug_monitor().await?;
//!
//! // Open the event stream *before* scanning: a stream opened later only sees the
//! // reports that arrive after it (bluer and the mock replay what they have already
//! // cached, btleplug does not).
//! let mut events = monitor.device_events().await?;
//!
//! // Start scanning for devices
//! monitor.start_scan().await?;
//!
//! while let Some(event) = events.next().await {
//!     println!("{event:?}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Reading a payload
//!
//! [`BluetoothDevice::raw_payload`] is not the same kind of thing on every backend:
//! btleplug never exposes the bytes, bluer rebuilds the structures BlueZ reported,
//! the mock carries what it was configured with.
//! [`BluetoothDevice::raw_payload_source`] says which of those a given value is, and
//! [`RawPayloadSource::Reconstructed`] means the payload's *silences* are not evidence
//! that an advertising structure was absent. See [`types`] for the table.

// Core modules
pub mod config;
pub mod error;
pub mod types;

// Re-export core types for convenience
pub use config::{adapter_matches, MonitorConfig, DEFAULT_SCAN_INTERVAL};
pub use error::{BackendKind, Error, Result};
pub use types::{
    BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId, GattCharacteristic,
    GattService, RawPayloadSource, ServiceUuid, ValueNotification,
};

// Monitor traits
pub mod monitor;

// Re-export traits for convenience
pub use monitor::{DeviceMonitor, GattClient, MonitorHealth};

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
/// This function is only available when the `bluer` feature is enabled. It needs
/// BlueZ 5.43+ and a reachable D-Bus, and it is Linux-only.
///
/// What it does differently from `btleplug`: it talks to BlueZ directly, so
/// `services_resolved` is BlueZ's own answer rather than a proxy for the link,
/// discovery is re-opened when the D-Bus subscription ends, and
/// [`raw_payload`](BluetoothDevice::raw_payload) carries the advertising structures
/// BlueZ reported — rebuilt from its typed properties and labelled
/// [`RawPayloadSource::Reconstructed`], because BlueZ does not hand over the bytes
/// and their on-air order is not among the things it says.
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
/// - Simulated GATT services and characteristics, with `allow_read` /
///   `allow_write` / `allow_subscribe` enforced in addition to the declared
///   properties
/// - Injected notifications
///   ([`send_notification`](backends::mock::MockMonitor::send_notification)) and an
///   optional automatic replay of configured values on a cadence
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
/// [`with_advertiser_count`](backends::mock::MockConfig::with_advertiser_count).
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
    /// Smoke test for the surface a consumer compiles against: the names re-exported here
    /// exist, are usable, and say what the documentation says they say. A re-export that
    /// disappears breaks downstream, not this crate, which is why it is checked at the root.
    #[test]
    fn the_crate_root_offers_what_it_promises() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<crate::MonitorConfig>();
        assert_send_sync::<crate::BluetoothDevice>();
        assert_eq!(
            crate::MonitorConfig::new().scan_interval,
            crate::DEFAULT_SCAN_INTERVAL,
            "the documented default cadence has to be the one a default configuration uses"
        );

        // `None` on both halves is the "this backend exposes no advertisement bytes"
        // spelling, and it is the shape a device starts in before any radio touches it.
        let device =
            crate::BluetoothDevice::new(crate::DeviceId::new("AA:BB:CC:DD:EE:FF"), String::new());
        assert_eq!(device.raw_payload, None);
        assert_eq!(device.raw_payload_source, None);
    }
}
