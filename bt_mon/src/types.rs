//! Core data types for bt_mon.
//!
//! This module provides backend-agnostic data types for representing
//! Bluetooth devices, services, characteristics, and notifications.
//!
//! # Types Overview
//!
//! - [`DeviceId`]: Unique identifier for a Bluetooth device
//! - [`ServiceUuid`], [`CharacteristicUuid`]: UUID wrappers for GATT objects
//! - [`BluetoothDevice`]: Represents a discovered Bluetooth device
//! - [`GattService`], [`GattCharacteristic`]: GATT service and characteristic
//! - [`CharacteristicProperties`]: Properties of a GATT characteristic
//! - [`ValueNotification`]: Notification from a characteristic
//! - [`UpdateField`]: Fields that can change in device updates

use std::collections::HashMap;
use std::fmt;

/// Unique identifier for a Bluetooth device.
///
/// This is a backend-agnostic string wrapper that uniquely identifies
/// a Bluetooth device. The format is platform-specific:
///
/// - On Linux/BlueZ: MAC address (e.g., "AA:BB:CC:DD:EE:FF")
/// - On macOS: UUID string
/// - On Windows: Platform-specific identifier
///
/// # Example
///
/// ```no_run
/// use bt_mon::DeviceId;
///
/// let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
/// assert_eq!(id.as_str(), "AA:BB:CC:DD:EE:FF");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(pub String);

impl DeviceId {
    /// Create a new device ID from a string.
    ///
    /// The string format is platform-specific (MAC address on Linux,
    /// UUID on macOS, etc.).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use bt_mon::DeviceId;
    /// let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
    /// ```
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Get the underlying string representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// UUID for a GATT service.
///
/// A type-safe wrapper around `uuid::Uuid` for GATT services.
/// Use this to ensure you don't accidentally mix up service UUIDs
/// with characteristic UUIDs.
///
/// # Example
///
/// ```no_run
/// use bt_mon::ServiceUuid;
///
/// let uuid = ServiceUuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap();
/// println!("Service UUID: {}", uuid);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServiceUuid(pub uuid::Uuid);

impl ServiceUuid {
    /// Create a new service UUID.
    pub fn new(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    /// Parse a UUID from a string.
    pub fn parse_str(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self(uuid::Uuid::parse_str(s)?))
    }

    /// Get the underlying UUID.
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }

    /// Get the string representation of the UUID.
    #[allow(clippy::inherent_to_string_shadow_display)]
    pub fn to_string(&self) -> String {
        self.0.to_string()
    }
}

impl fmt::Display for ServiceUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// UUID for a GATT characteristic.
///
/// A type-safe wrapper around `uuid::Uuid` for GATT characteristics.
/// Use this to ensure you don't accidentally mix up characteristic UUIDs
/// with service UUIDs.
///
/// # Example
///
/// ```no_run
/// use bt_mon::CharacteristicUuid;
///
/// let uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
/// println!("Characteristic UUID: {}", uuid);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CharacteristicUuid(pub uuid::Uuid);

impl CharacteristicUuid {
    /// Create a new characteristic UUID.
    pub fn new(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    /// Parse a UUID from a string.
    pub fn parse_str(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self(uuid::Uuid::parse_str(s)?))
    }

    /// Get the underlying UUID.
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }

    /// Get the string representation of the UUID.
    #[allow(clippy::inherent_to_string_shadow_display)]
    pub fn to_string(&self) -> String {
        self.0.to_string()
    }
}

impl fmt::Display for CharacteristicUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Represents a discovered Bluetooth device.
///
/// This struct contains all the information about a Bluetooth device
/// that can be obtained through scanning or connection.
///
/// # Example
///
/// ```no_run
/// use bt_mon::{BluetoothDevice, DeviceId};
///
/// let device = BluetoothDevice::new(
///     DeviceId::new("AA:BB:CC:DD:EE:FF"),
///     "AA:BB:CC:DD:EE:FF".to_string(),
/// )
/// .with_name("My Device")
/// .with_rssi(-50);
///
/// assert_eq!(device.name, Some("My Device".to_string()));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct BluetoothDevice {
    /// Unique identifier for the device.
    pub id: DeviceId,
    /// MAC address or platform-specific address.
    pub address: String,
    /// Local name from advertisement (may not be available).
    pub name: Option<String>,
    /// Received signal strength indicator.
    pub rssi: Option<i32>,
    /// Connection status.
    pub is_connected: bool,
    /// Manufacturer data from advertisement.
    pub manufacturer_data: HashMap<u16, Vec<u8>>,
    /// Service data from advertisement.
    pub service_data: HashMap<ServiceUuid, Vec<u8>>,
    /// Whether services have been resolved.
    pub services_resolved: bool,
}

impl BluetoothDevice {
    /// Create a new Bluetooth device.
    ///
    /// Creates a device with the given ID and address, with all optional
    /// fields initialized to empty/false values.
    pub fn new(id: DeviceId, address: String) -> Self {
        Self {
            id,
            address,
            name: None,
            rssi: None,
            is_connected: false,
            manufacturer_data: HashMap::new(),
            service_data: HashMap::new(),
            services_resolved: false,
        }
    }

    /// Set the device name.
    ///
    /// The name is typically obtained from the advertisement data.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the RSSI value.
    ///
    /// RSSI (Received Signal Strength Indicator) is measured in dBm.
    /// Typical values range from -100 (very weak) to 0 (very strong).
    pub fn with_rssi(mut self, rssi: i32) -> Self {
        self.rssi = Some(rssi);
        self
    }

    /// Set the connection status.
    pub fn with_connected(mut self, connected: bool) -> Self {
        self.is_connected = connected;
        self
    }

    /// Add manufacturer data.
    ///
    /// Manufacturer data is included in advertisement packets and
    /// contains a company identifier and vendor-specific data.
    pub fn with_manufacturer_data(mut self, company_id: u16, data: Vec<u8>) -> Self {
        self.manufacturer_data.insert(company_id, data);
        self
    }

    /// Add service data.
    ///
    /// Service data is included in advertisement packets and
    /// contains UUID-keyed service-specific data.
    pub fn with_service_data(mut self, uuid: ServiceUuid, data: Vec<u8>) -> Self {
        self.service_data.insert(uuid, data);
        self
    }
}

/// Represents a GATT service.
///
/// A GATT service is a collection of related characteristics.
/// Services can be primary (defined by the device) or secondary
/// (added to extend functionality).
///
/// # Example
///
/// ```no_run
/// use bt_mon::{GattService, ServiceUuid, GattCharacteristic, CharacteristicUuid, CharacteristicProperties};
///
/// let service_uuid = ServiceUuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap();
/// let char_uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
///
/// let service = GattService::new(service_uuid, true)
///     .with_characteristic(GattCharacteristic::new(char_uuid, CharacteristicProperties::default()));
///
/// assert_eq!(service.characteristics.len(), 1);
/// ```
#[derive(Clone, Debug)]
pub struct GattService {
    /// Service UUID.
    pub uuid: ServiceUuid,
    /// Whether this is a primary service.
    pub is_primary: bool,
    /// Characteristics belonging to this service.
    pub characteristics: Vec<GattCharacteristic>,
}

impl GattService {
    /// Create a new GATT service.
    ///
    /// # Arguments
    ///
    /// * `uuid` - The UUID of the service
    /// * `is_primary` - Whether this is a primary service
    pub fn new(uuid: ServiceUuid, is_primary: bool) -> Self {
        Self {
            uuid,
            is_primary,
            characteristics: Vec::new(),
        }
    }

    /// Add a characteristic to this service.
    ///
    /// Uses the builder pattern to allow chaining:
    ///
    /// ```no_run
    /// # use bt_mon::*;
    /// # let service_uuid = ServiceUuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap();
    /// # let char_uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
    /// let service = GattService::new(service_uuid, true)
    ///     .with_characteristic(GattCharacteristic::new(char_uuid, CharacteristicProperties::default()));
    /// ```
    pub fn with_characteristic(mut self, characteristic: GattCharacteristic) -> Self {
        self.characteristics.push(characteristic);
        self
    }
}

/// Represents a GATT characteristic.
///
/// A GATT characteristic holds the actual data that can be
/// read, written, or subscribed to for notifications.
///
/// # Example
///
/// ```no_run
/// use bt_mon::{CharacteristicUuid, GattCharacteristic, CharacteristicProperties};
///
/// let uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
/// let props = CharacteristicProperties::default()
///     .with_read(true)
///     .with_notify(true);
///
/// let char = GattCharacteristic::new(uuid, props);
/// assert!(char.can_read());
/// assert!(char.can_notify());
/// ```
#[derive(Clone, Debug)]
pub struct GattCharacteristic {
    /// Characteristic UUID.
    pub uuid: CharacteristicUuid,
    /// Characteristic properties.
    pub properties: CharacteristicProperties,
    /// Optional backend-specific handle.
    pub handle: Option<u16>,
}

impl GattCharacteristic {
    /// Create a new GATT characteristic.
    ///
    /// # Arguments
    ///
    /// * `uuid` - The UUID of the characteristic
    /// * `properties` - The properties of the characteristic
    pub fn new(uuid: CharacteristicUuid, properties: CharacteristicProperties) -> Self {
        Self {
            uuid,
            properties,
            handle: None,
        }
    }

    /// Set the characteristic handle.
    ///
    /// Handles are backend-specific numeric identifiers.
    pub fn with_handle(mut self, handle: u16) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Check if the characteristic supports reading.
    pub fn can_read(&self) -> bool {
        self.properties.read
    }

    /// Check if the characteristic supports writing.
    ///
    /// Returns true if either write-with-response or
    /// write-without-response is supported.
    pub fn can_write(&self) -> bool {
        self.properties.write || self.properties.write_without_response
    }

    /// Check if the characteristic supports notifications.
    ///
    /// Returns true if either notify or indicate is supported.
    pub fn can_notify(&self) -> bool {
        self.properties.notify || self.properties.indicate
    }
}

/// Properties of a GATT characteristic.
///
/// These properties indicate what operations can be performed on
/// a characteristic. They are read from the Characteristic Descriptor.
///
/// # Example
///
/// ```no_run
/// use bt_mon::CharacteristicProperties;
///
/// let props = CharacteristicProperties::default()
///     .with_read(true)
///     .with_write(true)
///     .with_notify(true);
///
/// assert!(props.read);
/// assert!(props.notify);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CharacteristicProperties {
    /// Supports broadcast.
    pub broadcast: bool,
    /// Supports read.
    pub read: bool,
    /// Supports write without response.
    pub write_without_response: bool,
    /// Supports write with response.
    pub write: bool,
    /// Supports notify.
    pub notify: bool,
    /// Supports indicate.
    pub indicate: bool,
    /// Supports authenticated signed writes.
    pub authenticated_signed_write: bool,
    /// Supports extended properties.
    pub extended_properties: bool,
}

impl CharacteristicProperties {
    /// Create a new property descriptor with all fields disabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the broadcast property.
    pub fn with_broadcast(mut self, broadcast: bool) -> Self {
        self.broadcast = broadcast;
        self
    }

    /// Set the read property.
    pub fn with_read(mut self, read: bool) -> Self {
        self.read = read;
        self
    }

    /// Set the write without response property.
    pub fn with_write_without_response(mut self, write: bool) -> Self {
        self.write_without_response = write;
        self
    }

    /// Set the write property.
    pub fn with_write(mut self, write: bool) -> Self {
        self.write = write;
        self
    }

    /// Set the notify property.
    pub fn with_notify(mut self, notify: bool) -> Self {
        self.notify = notify;
        self
    }

    /// Set the indicate property.
    pub fn with_indicate(mut self, indicate: bool) -> Self {
        self.indicate = indicate;
        self
    }

    /// Set the authenticated signed write property.
    pub fn with_authenticated_signed_write(mut self, authenticated_signed_write: bool) -> Self {
        self.authenticated_signed_write = authenticated_signed_write;
        self
    }

    /// Set the extended properties flag.
    pub fn with_extended_properties(mut self, extended_properties: bool) -> Self {
        self.extended_properties = extended_properties;
        self
    }
}

/// A value notification from a characteristic.
///
/// This struct contains the data sent to a client when a characteristic
/// value changes and the client has subscribed to notifications or indications.
///
/// # Example
///
/// ```no_run
/// use bt_mon::{ValueNotification, CharacteristicUuid};
///
/// let uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
/// let notification = ValueNotification::new(uuid, vec![0x48, 0x65, 0x6C, 0x6C, 0x6F]);
///
/// assert_eq!(notification.as_slice(), b"Hello");
/// ```
#[derive(Clone, Debug)]
pub struct ValueNotification {
    /// The characteristic that sent the notification.
    pub characteristic: CharacteristicUuid,
    /// The notification value.
    pub value: Vec<u8>,
    /// Optional timestamp when the notification was received.
    pub timestamp: Option<std::time::Instant>,
}

impl ValueNotification {
    /// Create a new value notification.
    ///
    /// Automatically sets the timestamp to the current time.
    pub fn new(characteristic: CharacteristicUuid, value: Vec<u8>) -> Self {
        Self {
            characteristic,
            value,
            timestamp: Some(std::time::Instant::now()),
        }
    }

    /// Create a notification without a timestamp.
    ///
    /// Useful for testing or when the timestamp is not relevant.
    pub fn without_timestamp(characteristic: CharacteristicUuid, value: Vec<u8>) -> Self {
        Self {
            characteristic,
            value,
            timestamp: None,
        }
    }

    /// Get the notification value as a slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.value
    }

    /// Convert the notification value to a new Vec.
    pub fn to_vec(&self) -> Vec<u8> {
        self.value.clone()
    }
}

/// Fields that can be updated in a device event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateField {
    /// Device name changed.
    Name,
    /// RSSI value changed.
    Rssi,
    /// Services resolved state changed.
    ServicesResolved,
    /// Connection state changed.
    Connected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_id_display() {
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        assert_eq!(format!("{}", id), "AA:BB:CC:DD:EE:FF");
        assert_eq!(id.as_str(), "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn test_device_id_new() {
        let id = DeviceId::new("1234");
        assert_eq!(id.0, "1234");
    }

    #[test]
    fn test_service_uuid() {
        let uuid = ServiceUuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap();
        assert_eq!(uuid.to_string(), "00001800-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn test_service_uuid_invalid() {
        let result = ServiceUuid::parse_str("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_characteristic_uuid() {
        let uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
        assert_eq!(uuid.to_string(), "00002a00-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn test_bluetooth_device_new() {
        let device = BluetoothDevice::new(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            "AA:BB:CC:DD:EE:FF".to_string(),
        );
        assert_eq!(device.id.as_str(), "AA:BB:CC:DD:EE:FF");
        assert!(!device.is_connected);
        assert!(device.name.is_none());
        assert!(device.rssi.is_none());
    }

    #[test]
    fn test_bluetooth_device_builder() {
        let device = BluetoothDevice::new(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            "AA:BB:CC:DD:EE:FF".to_string(),
        )
        .with_name("Test Device")
        .with_rssi(-50)
        .with_connected(true);

        assert_eq!(device.name, Some("Test Device".to_string()));
        assert_eq!(device.rssi, Some(-50));
        assert!(device.is_connected);
    }

    #[test]
    fn test_gatt_service() {
        let service_uuid = ServiceUuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap();
        let service = GattService::new(service_uuid, true);
        assert!(service.is_primary);
        assert!(service.characteristics.is_empty());
    }

    #[test]
    fn test_gatt_characteristic() {
        let char_uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
        let props = CharacteristicProperties::new()
            .with_read(true)
            .with_write(true)
            .with_notify(true);
        let char = GattCharacteristic::new(char_uuid, props).with_handle(0x0010);

        assert!(char.can_read());
        assert!(char.can_write());
        assert!(char.can_notify());
        assert_eq!(char.handle, Some(0x0010));
    }

    #[test]
    fn test_characteristic_properties_default() {
        let props = CharacteristicProperties::default();
        assert!(!props.read);
        assert!(!props.write);
        assert!(!props.notify);
        assert!(!props.indicate);
    }

    #[test]
    fn test_characteristic_properties_builder() {
        let props = CharacteristicProperties::new()
            .with_read(true)
            .with_write_without_response(true)
            .with_notify(true);

        assert!(props.read);
        assert!(props.write_without_response);
        assert!(props.notify);
        assert!(!props.write);
    }

    #[test]
    fn test_value_notification() {
        let char_uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
        let notification = ValueNotification::new(char_uuid, vec![1, 2, 3, 4]);

        assert_eq!(notification.characteristic, char_uuid);
        assert_eq!(notification.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(notification.to_vec(), vec![1, 2, 3, 4]);
        assert!(notification.timestamp.is_some());
    }

    #[test]
    fn test_value_notification_without_timestamp() {
        let char_uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
        let notification = ValueNotification::without_timestamp(char_uuid, vec![1, 2, 3]);

        assert!(notification.timestamp.is_none());
    }

    #[test]
    fn test_device_clone() {
        let device = BluetoothDevice::new(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            "AA:BB:CC:DD:EE:FF".to_string(),
        )
        .with_name("Test");
        let cloned = device.clone();
        assert_eq!(device.id, cloned.id);
        assert_eq!(device.name, cloned.name);
    }

    #[test]
    fn test_update_field_variants() {
        let fields = vec![
            UpdateField::Name,
            UpdateField::Rssi,
            UpdateField::ServicesResolved,
            UpdateField::Connected,
        ];
        assert_eq!(fields.len(), 4);
    }
}
