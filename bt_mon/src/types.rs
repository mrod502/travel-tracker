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

use std::collections::HashMap;
use std::fmt;

/// Unique identifier for a Bluetooth device.
///
/// This is a backend-agnostic string wrapper that uniquely identifies
/// a Bluetooth device. Backends report it in a platform-specific form, so
/// construct it with [`DeviceId::from_platform_id`] rather than from the raw
/// string unless the platform has already been accounted for:
///
/// - On Linux/BlueZ: the D-Bus object path tail, `hci0/dev_AA_BB_CC_DD_EE_FF`,
///   which carries the adapter the device was heard on
/// - On macOS: a host-local UUID — CoreBluetooth exposes no address to a central
/// - On Windows/Android: a bare colon-separated MAC address
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

    /// Normalise a backend's peripheral identifier into the form downstream
    /// consumers are told to expect: a bare, colon-separated, uppercase MAC
    /// address whenever the platform put one in the identifier at all.
    ///
    /// This exists because the adapter a device was heard on is not part of the
    /// device. BlueZ spells its `PeripheralId` `hci0/dev_F4_FE_FB_CC_78_58` — a
    /// slash, a `dev_` marker, and underscores — and an identifier hashed with
    /// the `hci0` still attached makes the same peripheral a different device on
    /// a node that has a second dongle plugged in, or on the same node after a
    /// reboot that renumbers the adapters.
    ///
    /// What it does, in order:
    ///
    /// 1. drop everything up to and including the last `/` (the D-Bus object
    ///    path's adapter component), so the adapter cannot survive into the
    ///    identifier even in a spelling this function has not been shown;
    /// 2. turn `dev_XX_XX_XX_XX_XX_XX` into `XX:XX:XX:XX:XX:XX`;
    /// 3. strip an `hci<digits>:` prefix, the spelling the `btleplug` backend's
    ///    own comment originally claimed.
    ///
    /// Anything else passes through: a CoreBluetooth UUID has no address to
    /// extract — Apple exposes none to a central — and Windows and Android
    /// already report a bare MAC. An identifier that still is not a MAC after
    /// this is the consumer's signal that it has no address, not a format error.
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::DeviceId;
    ///
    /// // The identifier btleplug actually emits on BlueZ.
    /// assert_eq!(
    ///     DeviceId::from_platform_id("hci0/dev_F4_FE_FB_CC_78_58").as_str(),
    ///     "F4:FE:FB:CC:78:58",
    /// );
    /// // A CoreBluetooth UUID: unchanged, because there is no address in it.
    /// assert_eq!(
    ///     DeviceId::from_platform_id("9cc22dd0-ff98-0ed2-b717-fe430544851b").as_str(),
    ///     "9cc22dd0-ff98-0ed2-b717-fe430544851b",
    /// );
    /// ```
    pub fn from_platform_id(raw: &str) -> Self {
        Self(normalize_platform_id(raw))
    }
}

/// See [`DeviceId::from_platform_id`] for what each step is for.
fn normalize_platform_id(raw: &str) -> String {
    // The adapter lives in front of the last slash, so dropping the prefix is
    // what keeps it out of every branch below — including a future spelling whose
    // tail this function does not recognise.
    let tail = raw
        .rsplit_once('/')
        .map_or(raw, |(_, tail)| if tail.is_empty() { raw } else { tail });

    if let Some(mac) = bluez_dev_marker(tail) {
        return mac;
    }

    if let Some((prefix, rest)) = tail.split_once(':') {
        if is_adapter_name(prefix) && !rest.is_empty() {
            return rest.to_uppercase();
        }
    }

    tail.to_string()
}

/// `dev_XX_XX_XX_XX_XX_XX` as `XX:XX:XX:XX:XX:XX`, requiring six pairs of hex
/// digits so a device whose name happens to start with `dev_` is not mistaken
/// for an address.
fn bluez_dev_marker(tail: &str) -> Option<String> {
    let groups: Vec<&str> = tail.strip_prefix("dev_")?.split('_').collect();
    if groups.len() != 6 {
        return None;
    }

    let mut mac = String::with_capacity(17);
    for group in groups {
        // The same two-hex-digit check `app`'s MAC parser applies, so a group
        // this accepts is one that parser will not reject afterwards.
        let mut chars = group.chars();
        match (chars.next(), chars.next(), chars.next()) {
            (Some(hi), Some(lo), None) if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() => {}
            _ => return None,
        }
        if !mac.is_empty() {
            mac.push(':');
        }
        mac.push_str(&group.to_ascii_uppercase());
    }
    Some(mac)
}

/// `hci0`, `hci1`, … — and not a hostname that merely starts with those letters.
fn is_adapter_name(prefix: &str) -> bool {
    prefix
        .strip_prefix("hci")
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The conversion callers reach for when they already own the address string —
/// `bluer` reports a `SimpleAddress` and converts with `.into()`, so without this
/// the `bluer` backend does not compile at all.
impl From<String> for DeviceId {
    fn from(id: String) -> Self {
        Self(id)
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
    /// The advertisement payload, as advertisement structures, from the data this
    /// backend's stack reported.
    ///
    /// Distinct from [`manufacturer_data`](Self::manufacturer_data) and
    /// [`service_data`](Self::service_data), which are the fields the backend
    /// parsed *out* of these bytes. What can be in here is decided by what the
    /// stack hands over, and it differs per backend:
    ///
    /// | backend | `raw_payload` |
    /// |---|---|
    /// | `btleplug` | always `None` — `PeripheralProperties` carries parsed fields only, so there are no per-AD-type bytes to re-frame and no raw buffer |
    /// | `bluer` | the advertisement re-framed as Length-Type-Data structures, built from the AD types BlueZ reported (`AdvertisingFlags`, `AdvertisingData`, `ManufacturerData`, `ServiceData`) |
    /// | `mock` | the bytes the simulated advertiser says it put on the air |
    ///
    /// So `None` means "this backend does not expose the advertisement contents",
    /// never "the device advertised nothing".
    ///
    /// Two limits on the `bluer` form, because a stored payload gets read back as
    /// evidence later:
    ///
    /// * **It is re-framed, not captured.** BlueZ parses the on-air buffer, so the
    ///   structures are re-assembled from its properties and their *order* need not
    ///   match the air. Byte equality between two payloads is meaningful only as
    ///   "this device's advertisement changed", never as "these two reports came
    ///   from the same capture".
    /// * **A structure that is absent may be absent because the stack did not
    ///   report it.** BlueZ exposes only the AD types it parses, and the name is
    ///   deliberately left out of the payload — BlueZ's `Name` may come from GATT
    ///   resolution rather than the advertisement, and a structure invented from it
    ///   would be an advertisement that never happened. Consumers must treat a
    ///   missing AD type in a `bluer` payload as *unknown*, not as proof the device
    ///   omitted it.
    pub raw_payload: Option<Vec<u8>>,
    /// Where [`raw_payload`](Self::raw_payload) came from, and `None` exactly when
    /// `raw_payload` is `None`.
    ///
    /// This exists because a stored advertisement gets read back as evidence. A
    /// payload re-framed from a stack's parsed properties says something different
    /// from one the radio handed over: in the re-framed form an AD type that is not
    /// in it may be missing because the stack never reported it, so a consumer that
    /// reasons from *absence* has to know which kind of payload it is holding. That
    /// cannot be recovered from the bytes, and it is not recoverable from "which
    /// backend was compiled in" once the row has crossed a federated link, so it
    /// travels with the payload.
    ///
    /// Set only by [`with_raw_payload`](Self::with_raw_payload) (captured) and
    /// [`with_reconstructed_raw_payload`](Self::with_reconstructed_raw_payload), so
    /// the two fields cannot disagree. Not a tracked field: a backend cannot change
    /// where its data came from part-way through a stream, so this never appears in
    /// [`UpdateField`](crate::monitor::events::UpdateField).
    pub raw_payload_source: Option<RawPayloadSource>,
}

/// Where a device's [`BluetoothDevice::raw_payload`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RawPayloadSource {
    /// The octets as the radio reported them. Every AD type the device put on the
    /// air is in here, and one that is absent was not advertised.
    Captured,
    /// Re-framed as Length-Type-Data structures from the advertisement properties
    /// the stack exposed — see [`BluetoothDevice::raw_payload`] for the two limits
    /// that follow (order is not the on-air order; an absent AD type is unknown,
    /// not evidence of absence).
    ///
    /// A consumer that scores the *presence* of an AD structure may use this. A
    /// consumer that scores its absence must not.
    Reconstructed,
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
            raw_payload: None,
            raw_payload_source: None,
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

    /// Attach advertisement bytes the radio reported, labelled
    /// [`RawPayloadSource::Captured`].
    ///
    /// Use this only for bytes that *are* the advertisement. A payload re-assembled
    /// from a stack's parsed properties is
    /// [`with_reconstructed_raw_payload`](Self::with_reconstructed_raw_payload), and
    /// the difference is what lets a consumer downstream tell an absent AD structure
    /// from one the stack never reported — see
    /// [`BluetoothDevice::raw_payload`](Self::raw_payload).
    pub fn with_raw_payload(mut self, payload: impl Into<Vec<u8>>) -> Self {
        self.raw_payload = Some(payload.into());
        self.raw_payload_source = Some(RawPayloadSource::Captured);
        self
    }

    /// Attach advertisement bytes re-framed from the properties the stack reported,
    /// labelled [`RawPayloadSource::Reconstructed`].
    pub fn with_reconstructed_raw_payload(mut self, payload: impl Into<Vec<u8>>) -> Self {
        self.raw_payload = Some(payload.into());
        self.raw_payload_source = Some(RawPayloadSource::Reconstructed);
        self
    }
}

/// One `Length-Type-Data` advertisement structure, as it goes over the air.
///
/// `None` when the data cannot be expressed as one structure: the length byte
/// counts itself and the type, so 254 bytes of payload is the most that fits. A
/// caller building a payload from stack-reported properties skips such a structure
/// rather than truncating it, because a half-copied AD type is a lie about what the
/// device advertised.
#[cfg(any(feature = "bluer", feature = "mock"))]
pub(crate) fn ad_structure(kind: u8, data: &[u8]) -> Option<Vec<u8>> {
    if data.len() > 254 {
        return None;
    }

    let mut out = Vec::with_capacity(data.len() + 2);
    out.push((data.len() + 1) as u8);
    out.push(kind);
    out.extend_from_slice(data);
    Some(out)
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
    /// The service containing [`characteristic`](Self::characteristic), when the
    /// backend can attribute the frame to one.
    ///
    /// [`GattClient::notifications`](crate::monitor::GattClient::notifications) is
    /// per device and carries every characteristic subscribed on it, so a consumer
    /// multiplexing several subscriptions needs this to tell frames apart when two
    /// services expose the same characteristic uuid. Each backend fills it from
    /// what its own stack reports: btleplug from the service uuid on the frame,
    /// bluer and the mock from the service the subscription was opened through.
    pub service: Option<ServiceUuid>,
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
            service: None,
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
            service: None,
            value,
            timestamp: None,
        }
    }

    /// Name the service this frame came from.
    pub fn with_service(mut self, service: ServiceUuid) -> Self {
        self.service = Some(service);
        self
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

// `UpdateField` lives in `crate::monitor::events` and this module deliberately
// does not define its own copy. It used to: two enums with the same name were
// distinct types, so a consumer holding `bt_mon::UpdateField` could not match a
// value that arrived on `bt_mon::monitor::events::UpdateField`.

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
        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
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
        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
        let notification = ValueNotification::new(char_uuid, vec![1, 2, 3, 4]);

        assert_eq!(notification.characteristic, char_uuid);
        assert_eq!(notification.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(notification.to_vec(), vec![1, 2, 3, 4]);
        assert!(notification.timestamp.is_some());
    }

    #[test]
    fn test_value_notification_without_timestamp() {
        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
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

    /// The identifier captured on real BlueZ hardware on 2026-10-13 (M37), where
    /// every one of 173 rows hashed `hci0/dev_F4_FE_FB_CC_78_58` because nothing
    /// recognised this spelling. A btleplug bump that changes it again has to
    /// break this test rather than silently change what a node hashes.
    #[test]
    fn the_bluez_peripheral_id_becomes_a_bare_mac() {
        assert_eq!(
            DeviceId::from_platform_id("hci0/dev_F4_FE_FB_CC_78_58").as_str(),
            "F4:FE:FB:CC:78:58"
        );
        // The whole object path, in case a future Display stops stripping it.
        assert_eq!(
            DeviceId::from_platform_id("/org/bluez/hci0/dev_F4_FE_FB_CC_78_58").as_str(),
            "F4:FE:FB:CC:78:58"
        );
        // Lowercase from a stack that prints it that way is the same address.
        assert_eq!(
            DeviceId::from_platform_id("hci0/dev_f4_fe_fb_cc_78_58").as_str(),
            "F4:FE:FB:CC:78:58"
        );
    }

    /// The spelling `peripheral_id_to_device_id`'s own comment claimed, which the
    /// fix still has to honour: stripping the adapter prefix off a colon form.
    #[test]
    fn the_colon_spelling_the_old_comment_claimed_still_works() {
        assert_eq!(
            DeviceId::from_platform_id("hci0:AA:BB:CC:DD:EE:FF").as_str(),
            "AA:BB:CC:DD:EE:FF"
        );
        assert_eq!(
            DeviceId::from_platform_id("hci12:aa:bb:cc:dd:ee:ff").as_str(),
            "AA:BB:CC:DD:EE:FF"
        );
    }

    /// A CoreBluetooth identifier carries no address — Apple exposes none to a
    /// central — so rewriting it would invent one.
    #[test]
    fn a_core_bluetooth_uuid_passes_through_unchanged() {
        let uuid = "9cc22dd0-ff98-0ed2-b717-fe430544851b";
        assert_eq!(DeviceId::from_platform_id(uuid).as_str(), uuid);
    }

    /// An identifier with no address in it at all stays as it arrived, minus the
    /// adapter: the consumer's job is to report "no address", not to guess.
    #[test]
    fn an_identifier_with_no_address_passes_through_unchanged() {
        assert_eq!(
            DeviceId::from_platform_id("some-device-path").as_str(),
            "some-device-path"
        );
        assert_eq!(DeviceId::from_platform_id("").as_str(), "");
        // A Windows or Android id already arrives bare.
        assert_eq!(
            DeviceId::from_platform_id("AA:BB:CC:DD:EE:FF").as_str(),
            "AA:BB:CC:DD:EE:FF"
        );
    }

    /// The adapter a device was heard on must not survive into the identifier —
    /// which is the half of M37 that made one peripheral look like two devices.
    #[test]
    fn the_adapter_name_never_survives() {
        for adapter in ["hci0", "hci1", "hci37"] {
            assert_eq!(
                DeviceId::from_platform_id(&format!("{adapter}/dev_F4_FE_FB_CC_78_58")).as_str(),
                "F4:FE:FB:CC:78:58"
            );
        }
        // An unrecognised tail still loses the adapter rather than keeping it.
        assert_eq!(
            DeviceId::from_platform_id("hci0/something_else").as_str(),
            "something_else"
        );
    }

    /// `dev_` is a BlueZ marker, not a general prefix: accepting fewer than six
    /// pairs, or non-hex pairs, would turn a name into an address.
    #[test]
    fn a_partial_or_malformed_dev_marker_is_not_read_as_a_mac() {
        for raw in [
            "dev_F4_FE_FB_CC_78",
            "dev_F4_FE_FB_CC_78_58_99",
            "dev_F4_FE_FB_CC_78_ZZ",
            "dev_F4_FE_FB_CC_78_5",
            "device_not_an_address",
        ] {
            assert_eq!(
                DeviceId::from_platform_id(raw).as_str(),
                raw,
                "{raw} was rewritten into an address it does not contain"
            );
        }
    }

    /// `hci` as the start of a word is not an adapter number.
    #[test]
    fn only_a_numbered_adapter_prefix_is_stripped() {
        assert_eq!(
            DeviceId::from_platform_id("hciX:AA:BB:CC:DD:EE:FF").as_str(),
            "hciX:AA:BB:CC:DD:EE:FF"
        );
        assert_eq!(
            DeviceId::from_platform_id("hci:AA:BB:CC:DD:EE:FF").as_str(),
            "hci:AA:BB:CC:DD:EE:FF"
        );
    }
}
