//! Device monitoring and GATT client traits.
//!
//! This module provides the core traits for Bluetooth device monitoring
//! and GATT client operations.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{
    BluetoothDevice, CharacteristicUuid, DeviceId, GattService,
};
use crate::monitor::events::{DeviceEventStream, NotificationStream};

pub mod events;

pub use events::{DeviceEvent, NotificationEvent, UpdateField};

/// Device monitoring interface for discovering and tracking Bluetooth devices.
///
/// # Example
///
/// ```ignore
/// # async fn example() -> Result<(), bt_mon::Error> {
/// use bt_mon::{DeviceMonitor, DeviceId};
///
/// let monitor = bt_mon::create_btleplug_monitor().await?;
/// monitor.start_scan().await?;
///
/// let mut events = monitor.device_events().await?;
/// while let Some(event) = events.next().await {
///     println!("Device event: {:?}", event);
/// }
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait DeviceMonitor: Send + Sync {
    /// Start scanning for Bluetooth devices.
    ///
    /// # Errors
    ///
    /// Returns an error if scanning fails to start or if already scanning.
    async fn start_scan(&self) -> Result<()>;

    /// Stop scanning for Bluetooth devices.
    ///
    /// # Errors
    ///
    /// Returns an error if stopping scan fails or if not currently scanning.
    async fn stop_scan(&self) -> Result<()>;

    /// Get list of all discovered devices.
    ///
    /// # Errors
    ///
    /// Returns an error if device list cannot be retrieved.
    async fn devices(&self) -> Result<Vec<BluetoothDevice>>;

    /// Get a specific device by ID.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DeviceNotFound`](crate::Error::DeviceNotFound) if the device is not found.
    async fn device(&self, id: &DeviceId) -> Result<BluetoothDevice>;

    /// Check if adapter is powered on.
    ///
    /// # Errors
    ///
    /// Returns an error if adapter status cannot be determined.
    async fn is_powered(&self) -> Result<bool>;

    /// Get adapter information.
    ///
    /// # Errors
    ///
    /// Returns an error if adapter information cannot be retrieved.
    async fn adapter_info(&self) -> Result<String>;

    /// Get stream of device events.
    ///
    /// # Errors
    ///
    /// Returns an error if event stream cannot be created.
    async fn device_events(&self) -> Result<DeviceEventStream>;

    /// Check if currently scanning.
    ///
    /// # Errors
    ///
    /// Returns an error if scanning status cannot be determined.
    async fn is_scanning(&self) -> Result<bool>;
}

/// GATT client interface for interacting with connected devices.
///
/// # Example
///
/// ```ignore
/// # async fn example() -> Result<(), bt_mon::Error> {
/// use bt_mon::{GattClient, DeviceId, CharacteristicUuid};
///
/// let monitor = bt_mon::create_btleplug_monitor().await?;
/// let device_id = DeviceId::new("00:11:22:33:44:55");
///
/// // Connect and discover services
/// monitor.connect(&device_id).await?;
/// monitor.discover_services(&device_id).await?;
///
/// // Read a characteristic
/// let char_uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb")?;
/// let value = monitor.read_characteristic(&device_id, &char_uuid).await?;
///
/// // Subscribe to notifications
/// monitor.subscribe(&device_id, &char_uuid).await?;
/// let mut notifications = monitor.notifications(&device_id).await?;
/// while let Some(notification) = notifications.next().await {
///     println!("Notification: {:?}", notification);
/// }
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait GattClient: DeviceMonitor {
    /// Connect to a device.
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or times out.
    async fn connect(&self, id: &DeviceId) -> Result<()>;

    /// Disconnect from a device.
    ///
    /// # Errors
    ///
    /// Returns an error if disconnection fails.
    async fn disconnect(&self, id: &DeviceId) -> Result<()>;

    /// Check if device is connected.
    ///
    /// # Errors
    ///
    /// Returns an error if connection status cannot be determined.
    async fn is_connected(&self, id: &DeviceId) -> Result<bool>;

    /// Discover GATT services on a connected device.
    ///
    /// # Errors
    ///
    /// Returns an error if service discovery fails.
    async fn discover_services(&self, id: &DeviceId) -> Result<Vec<GattService>>;

    /// Get discovered services for a device.
    ///
    /// Returns previously discovered services without re-discovering.
    ///
    /// # Errors
    ///
    /// Returns an error if services have not been discovered yet.
    async fn services(&self, id: &DeviceId) -> Result<Vec<GattService>>;

    /// Read a characteristic value.
    ///
    /// # Errors
    ///
    /// Returns an error if the characteristic doesn't support reading,
    /// if the read operation fails, or if the device disconnects.
    async fn read_characteristic(&self, id: &DeviceId, uuid: &CharacteristicUuid)
        -> Result<Vec<u8>>;

    /// Write a characteristic value.
    ///
    /// # Arguments
    ///
    /// * `id` - The device to write to
    /// * `uuid` - The characteristic UUID
    /// * `value` - The value to write
    /// * `response` - Whether to wait for a write response
    ///
    /// # Errors
    ///
    /// Returns an error if the characteristic doesn't support writing,
    /// if the write operation fails, or if the device disconnects.
    async fn write_characteristic(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
        value: &[u8],
        response: bool,
    ) -> Result<()>;

    /// Subscribe to characteristic notifications.
    ///
    /// # Errors
    ///
    /// Returns an error if the characteristic doesn't support notifications,
    /// if the subscription fails, or if the device disconnects.
    async fn subscribe(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
    ) -> Result<()>;

    /// Unsubscribe from characteristic notifications.
    ///
    /// # Errors
    ///
    /// Returns an error if the unsubscribe operation fails.
    async fn unsubscribe(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
    ) -> Result<()>;

    /// Get notification stream for a device.
    ///
    /// # Errors
    ///
    /// Returns an error if the notification stream cannot be created.
    async fn notifications(&self, id: &DeviceId) -> Result<NotificationStream>;
}
