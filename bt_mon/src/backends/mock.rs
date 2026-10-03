//! Mock backend for testing and development.
//!
//! This module provides a fully configurable mock implementation of the
//! `DeviceMonitor` and `GattClient` traits for use in environments without
//! physical Bluetooth interfaces, or for testing purposes.
//!
//! # Features
//!
//! - **Configurable responses**: All trait members can be configured
//! - **Simulated device discovery**: Add/remove devices programmatically
//! - **Simulated GATT services**: Define custom services and characteristics
//! - **Simulated notifications**: Control notification behavior
//! - **Error simulation**: Configure error responses for testing error handling
//!
//! # Basic Usage
//!
//! ```
//! use bt_mon::{DeviceMonitor, GattClient, DeviceId, CharacteristicUuid};
//! use bt_mon::backends::mock::MockMonitor;
//! use uuid::Uuid;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), bt_mon::Error> {
//! // Create a mock monitor with default configuration
//! let monitor = MockMonitor::new();
//!
//! // Add a simulated device
//! let device_id = DeviceId::new("00:11:22:33:44:55");
//! monitor.add_device(device_id.clone()).await?;
//!
//! // Start scanning (simulated)
//! monitor.start_scan().await?;
//!
//! // Get discovered devices
//! let devices = monitor.devices().await?;
//! println!("Found {} devices", devices.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Advanced Configuration
//!
//! ```
//! use bt_mon::backends::mock::{MockMonitor, MockConfig};
//! use std::time::Duration;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), bt_mon::Error> {
//! // Create a mock monitor with custom configuration
//! let config = MockConfig::default()
//!     .with_scan_delay_ms(100)
//!     .with_read_delay_ms(50)
//!     .with_error_on_scan(true);
//!
//! let monitor = MockMonitor::with_config(config);
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use dashmap::DashMap;
use log::{debug, info};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio::time;

use crate::error::{Error, Result};
use crate::monitor::events::{
    DeviceEvent, DeviceEventStream, NotificationEvent, NotificationStream,
    DEVICE_EVENT_CHANNEL_CAPACITY,
};
use crate::monitor::{DeviceMonitor, GattClient};
use crate::types::{
    BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId, GattCharacteristic,
    GattService, ServiceUuid,
};

/// Configuration for the mock backend.
///
/// This struct allows you to customize the behavior of the mock monitor
/// for different testing scenarios.
#[derive(Debug, Clone)]
pub struct MockConfig {
    /// Delay before devices appear during scan (milliseconds).
    pub scan_delay_ms: u64,
    /// Delay for read operations (milliseconds).
    pub read_delay_ms: u64,
    /// Delay for write operations (milliseconds).
    pub write_delay_ms: u64,
    /// Delay for connection operations (milliseconds).
    pub connect_delay_ms: u64,
    /// Whether to simulate scan failure.
    pub error_on_scan: bool,
    /// Whether to simulate connection failure.
    pub error_on_connect: bool,
    /// Whether to simulate read failure.
    pub error_on_read: bool,
    /// Whether to simulate write failure.
    pub error_on_write: bool,
    /// Custom error message for scan failures.
    pub scan_error_message: String,
    /// Custom error message for connection failures.
    pub connect_error_message: String,
    /// Custom error message for read failures.
    pub read_error_message: String,
    /// Custom error message for write failures.
    pub write_error_message: String,
    /// Default RSSI value for simulated devices.
    pub default_rssi: i32,
    /// Whether to auto-power on the adapter.
    pub adapter_powered: bool,
    /// Adapter name to report.
    pub adapter_name: String,
    /// Services to auto-add to devices.
    pub default_services: Vec<SimulatedService>,
    /// Simulated notification values to send.
    pub notification_values: HashMap<CharacteristicUuid, Vec<Vec<u8>>>,
    /// Whether notifications should auto-send values.
    pub auto_send_notifications: bool,
    /// Interval between automatic notifications (milliseconds).
    pub notification_interval_ms: u64,
    /// Devices the radio advertises on its own while a scan is running.
    ///
    /// Empty by default: a monitor built for unit tests should present an empty
    /// room until the test puts a device in it with [`MockMonitor::add_device`].
    /// Anything here is broadcast as a `DeviceAdded` on
    /// [`MockConfig::advertise_interval_ms`] once `start_scan` succeeds, which is
    /// what a development run needs — without it `--use-mock-backend` opens a
    /// stream that never delivers anything, and the node scans successfully while
    /// storing zero occurrences.
    pub advertisers: Vec<SimulatedAdvertiser>,
    /// Interval between simulated advertisements (milliseconds).
    pub advertise_interval_ms: u64,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            scan_delay_ms: 100,
            read_delay_ms: 50,
            write_delay_ms: 50,
            connect_delay_ms: 200,
            error_on_scan: false,
            error_on_connect: false,
            error_on_read: false,
            error_on_write: false,
            scan_error_message: "Simulated scan error".to_string(),
            connect_error_message: "Simulated connection error".to_string(),
            read_error_message: "Simulated read error".to_string(),
            write_error_message: "Simulated write error".to_string(),
            default_rssi: -60,
            adapter_powered: true,
            adapter_name: "MockAdapter".to_string(),
            default_services: Vec::new(),
            notification_values: HashMap::new(),
            auto_send_notifications: false,
            notification_interval_ms: 1000,
            advertisers: Vec::new(),
            advertise_interval_ms: 1000,
        }
    }
}

/// A device the mock radio puts on the air by itself while a scan is running.
///
/// This is the difference between a mock *harness* — a monitor that reports an
/// empty room until a test adds a device to it — and a mock *environment* an
/// application can be started against: with nothing advertising, `--use-mock-backend`
/// opens a stream that never delivers an event, so the node reports a healthy
/// scan while storing no occurrences at all.
#[derive(Debug, Clone)]
pub struct SimulatedAdvertiser {
    /// The advertised address.
    pub id: DeviceId,
    /// The advertised name.
    pub name: String,
    /// Signal strength reported for each advertisement.
    pub rssi: i32,
}

impl SimulatedAdvertiser {
    /// An advertiser with the default RSSI.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: DeviceId::new(id),
            name: name.into(),
            rssi: -60,
        }
    }

    /// Set the signal strength reported for each advertisement.
    pub fn with_rssi(mut self, rssi: i32) -> Self {
        self.rssi = rssi;
        self
    }

    /// `count` advertisers with stable addresses (`F0:EE:…`) and names.
    ///
    /// Stable means two runs of the same command see the same devices, which is
    /// what makes a mock run comparable to the one before it; the node's rate
    /// limiter then does what it does with a device it has just heard from.
    pub fn sequence(count: usize) -> Vec<SimulatedAdvertiser> {
        (0..count)
            .map(|i| {
                SimulatedAdvertiser::new(
                    format!("F0:EE:00:00:{:02X}:{:02X}", i / 256, i % 256),
                    format!("Mock Beacon {i}"),
                )
            })
            .collect()
    }

    /// The advertisement this beacon puts on the air, as advertisement
    /// structures: flags, the complete local name, and a manufacturer-specific
    /// block carrying the beacon's own address.
    ///
    /// The point is that a consumer of [`BluetoothDevice::raw_payload`] has
    /// something real to store in a development run: the parsed fields say what
    /// the beacon advertised, and this is the byte string they were parsed from,
    /// in the same order every time, so a stored payload can be diffed against
    /// the previous run's.
    pub fn advertisement_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        // Flags: LE General Discoverable, no BR/EDR re-support.
        bytes.extend_from_slice(&ad_structure(0x01, &[0x06]));
        // Complete local name (AD forbids more than 30 bytes of payload).
        let name = self.name.as_bytes();
        bytes.extend_from_slice(&ad_structure(0x09, &name[..name.len().min(30)]));
        // Manufacturer-specific data: a reserved company id plus the address
        // bytes, which is what makes each beacon's payload distinct.
        let mut manufacturer = 0xFFFFu16.to_le_bytes().to_vec();
        manufacturer.extend_from_slice(&address_bytes(self.id.as_str()));
        bytes.extend_from_slice(&ad_structure(0xFF, &manufacturer));
        bytes
    }
}

/// One `Length-Type-Data` advertisement structure, as it goes over the air.
fn ad_structure(kind: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 2);
    out.push((data.len() + 1) as u8);
    out.push(kind);
    out.extend_from_slice(data);
    out
}

/// The six address bytes behind a MAC-ish identifier, or a stable stand-in.
///
/// A mock can be given any string as an identifier, and the manufacturer block
/// has to be six bytes whatever it is.
fn address_bytes(id: &str) -> [u8; 6] {
    let groups: Option<Vec<u8>> = id
        .split(':')
        .map(|group| u8::from_str_radix(group, 16).ok())
        .collect();

    match groups {
        Some(bytes) if bytes.len() == 6 => bytes
            .as_slice()
            .try_into()
            .expect("six groups collected above"),
        _ => {
            let mut out = [0u8; 6];
            for (slot, byte) in out.iter_mut().zip(id.as_bytes()) {
                *slot = *byte;
            }
            out
        }
    }
}

impl MockConfig {
    /// Set the scan delay in milliseconds.
    pub fn with_scan_delay_ms(mut self, delay: u64) -> Self {
        self.scan_delay_ms = delay;
        self
    }

    /// Set the read delay in milliseconds.
    pub fn with_read_delay_ms(mut self, delay: u64) -> Self {
        self.read_delay_ms = delay;
        self
    }

    /// Set the write delay in milliseconds.
    pub fn with_write_delay_ms(mut self, delay: u64) -> Self {
        self.write_delay_ms = delay;
        self
    }

    /// Set the connection delay in milliseconds.
    pub fn with_connect_delay_ms(mut self, delay: u64) -> Self {
        self.connect_delay_ms = delay;
        self
    }

    /// Enable scan error simulation.
    pub fn with_error_on_scan(mut self, error: bool) -> Self {
        self.error_on_scan = error;
        self
    }

    /// Enable connection error simulation.
    pub fn with_error_on_connect(mut self, error: bool) -> Self {
        self.error_on_connect = error;
        self
    }

    /// Enable read error simulation.
    pub fn with_error_on_read(mut self, error: bool) -> Self {
        self.error_on_read = error;
        self
    }

    /// Enable write error simulation.
    pub fn with_error_on_write(mut self, error: bool) -> Self {
        self.error_on_write = error;
        self
    }

    /// Set the scan error message.
    pub fn with_scan_error_message(mut self, message: impl Into<String>) -> Self {
        self.scan_error_message = message.into();
        self
    }

    /// Set the connection error message.
    pub fn with_connect_error_message(mut self, message: impl Into<String>) -> Self {
        self.connect_error_message = message.into();
        self
    }

    /// Set the read error message.
    pub fn with_read_error_message(mut self, message: impl Into<String>) -> Self {
        self.read_error_message = message.into();
        self
    }

    /// Set the write error message.
    pub fn with_write_error_message(mut self, message: impl Into<String>) -> Self {
        self.write_error_message = message.into();
        self
    }

    /// Set the default RSSI value.
    pub fn with_default_rssi(mut self, rssi: i32) -> Self {
        self.default_rssi = rssi;
        self
    }

    /// Set whether the adapter is powered.
    pub fn with_adapter_powered(mut self, powered: bool) -> Self {
        self.adapter_powered = powered;
        self
    }

    /// Set the adapter name.
    pub fn with_adapter_name(mut self, name: impl Into<String>) -> Self {
        self.adapter_name = name.into();
        self
    }

    /// Add a default service to devices.
    pub fn with_default_service(mut self, service: SimulatedService) -> Self {
        self.default_services.push(service);
        self
    }

    /// Add a notification value for a characteristic.
    pub fn with_notification_value(mut self, uuid: CharacteristicUuid, value: Vec<u8>) -> Self {
        self.notification_values
            .entry(uuid)
            .or_default()
            .push(value);
        self
    }

    /// Enable automatic notification sending.
    pub fn with_auto_send_notifications(mut self, enabled: bool) -> Self {
        self.auto_send_notifications = enabled;
        self
    }

    /// Set the notification interval in milliseconds.
    pub fn with_notification_interval_ms(mut self, interval: u64) -> Self {
        self.notification_interval_ms = interval;
        self
    }

    /// Advertise these devices whenever a scan is running.
    pub fn with_advertisers(mut self, advertisers: Vec<SimulatedAdvertiser>) -> Self {
        self.advertisers = advertisers;
        self
    }

    /// Advertise `count` generated devices whenever a scan is running.
    ///
    /// This is the switch that turns the mock from a harness a test drives into
    /// an environment `app --use-mock-backend` can be started against: an
    /// unattended run has to have something to discover, or it stores nothing
    /// and looks identical to a working node.
    pub fn with_advertiser_count(mut self, count: usize) -> Self {
        self.advertisers = SimulatedAdvertiser::sequence(count);
        self
    }

    /// Set the interval between simulated advertisements in milliseconds.
    pub fn with_advertise_interval_ms(mut self, interval: u64) -> Self {
        self.advertise_interval_ms = interval;
        self
    }
}

/// A simulated GATT service.
#[derive(Debug, Clone)]
pub struct SimulatedService {
    /// Service UUID.
    pub uuid: ServiceUuid,
    /// Whether this is a primary service.
    pub is_primary: bool,
    /// Characteristics in this service.
    pub characteristics: Vec<SimulatedCharacteristic>,
}

impl SimulatedService {
    /// Create a new simulated service.
    pub fn new(uuid: ServiceUuid, is_primary: bool) -> Self {
        Self {
            uuid,
            is_primary,
            characteristics: Vec::new(),
        }
    }

    /// Add a characteristic to this service.
    pub fn with_characteristic(mut self, char: SimulatedCharacteristic) -> Self {
        self.characteristics.push(char);
        self
    }
}

/// A simulated GATT characteristic.
#[derive(Debug, Clone)]
pub struct SimulatedCharacteristic {
    /// Characteristic UUID.
    pub uuid: CharacteristicUuid,
    /// Characteristic properties.
    pub properties: CharacteristicProperties,
    /// Simulated handle.
    pub handle: Option<u16>,
    /// Read value.
    pub read_value: Vec<u8>,
    /// Whether read is allowed.
    pub allow_read: bool,
    /// Whether write is allowed.
    pub allow_write: bool,
    /// Whether subscribe is allowed.
    pub allow_subscribe: bool,
}

impl SimulatedCharacteristic {
    /// Create a new simulated characteristic.
    pub fn new(uuid: CharacteristicUuid, properties: CharacteristicProperties) -> Self {
        Self {
            uuid,
            properties,
            handle: None,
            read_value: Vec::new(),
            allow_read: true,
            allow_write: true,
            allow_subscribe: true,
        }
    }

    /// Set the handle.
    pub fn with_handle(mut self, handle: u16) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Set the read value.
    pub fn with_read_value(mut self, value: Vec<u8>) -> Self {
        self.read_value = value;
        self
    }

    /// Set whether read is allowed.
    pub fn with_allow_read(mut self, allowed: bool) -> Self {
        self.allow_read = allowed;
        self
    }

    /// Set whether write is allowed.
    pub fn with_allow_write(mut self, allowed: bool) -> Self {
        self.allow_write = allowed;
        self
    }

    /// Set whether subscribe is allowed.
    pub fn with_allow_subscribe(mut self, allowed: bool) -> Self {
        self.allow_subscribe = allowed;
        self
    }
}

/// Internal representation of a simulated device.
#[derive(Clone)]
struct SimulatedDevice {
    device: BluetoothDevice,
    services: Vec<GattService>,
    subscribed_characteristics: Arc<RwLock<HashMap<CharacteristicUuid, bool>>>,
}

impl SimulatedDevice {
    /// Create a new simulated device.
    fn new(device: BluetoothDevice) -> Self {
        Self {
            device,
            services: Vec::new(),
            subscribed_characteristics: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

/// Mock backend implementation of [`DeviceMonitor`] and [`GattClient`].
///
/// This implementation provides a fully configurable mock for testing and
/// development purposes. All responses can be customized through [`MockConfig`].
///
/// # Example
///
/// ```
/// use bt_mon::{DeviceMonitor, GattClient, DeviceId, CharacteristicUuid};
/// use bt_mon::backends::mock::{MockMonitor, MockConfig, SimulatedService, SimulatedCharacteristic};
/// use bt_mon::types::{ServiceUuid, GattService, CharacteristicProperties};
/// use uuid::Uuid;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), bt_mon::Error> {
/// // Create a mock monitor with custom configuration
/// let config = MockConfig::default()
///     .with_adapter_name("TestAdapter")
///     .with_default_rssi(-50);
///
/// let monitor = MockMonitor::with_config(config);
///
/// // Add a device
/// let device_id = DeviceId::new("AA:BB:CC:DD:EE:FF");
/// monitor.add_device(device_id.clone()).await?;
///
/// // Start scanning
/// monitor.start_scan().await?;
///
/// // Connect to the device
/// monitor.connect(&device_id).await?;
///
/// // Discover services
/// let services = monitor.discover_services(&device_id).await?;
///
/// Ok(())
/// # }
/// ```
pub struct MockMonitor {
    config: MockConfig,
    devices: Arc<DashMap<DeviceId, SimulatedDevice>>,
    scanning: Arc<Mutex<bool>>,
    /// Liveness flag for the advertiser loop, so dropping the monitor stops the
    /// task it spawned rather than leaving it broadcasting to nobody.
    advertising: Arc<AtomicBool>,
    notification_senders: Arc<DashMap<DeviceId, tokio::sync::mpsc::Sender<NotificationEvent>>>,
    event_senders: Arc<DashMap<u64, futures::channel::mpsc::Sender<DeviceEvent>>>,
    next_event_sender_id: Arc<AtomicU64>,
}

impl MockMonitor {
    /// Create a new mock monitor with default configuration.
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::backends::mock::MockMonitor;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let monitor = MockMonitor::new();
    /// # Ok(())
    /// # }
    /// ```
    pub fn new() -> Self {
        Self::with_config(MockConfig::default())
    }

    /// Create a new mock monitor with custom configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - The configuration for the mock monitor
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::backends::mock::{MockMonitor, MockConfig};
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let config = MockConfig::default()
    ///     .with_error_on_scan(false)
    ///     .with_adapter_name("MyMockAdapter");
    ///
    /// let monitor = MockMonitor::with_config(config);
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_config(config: MockConfig) -> Self {
        debug!("Creating mock monitor with config: {:?}", config);
        Self {
            config,
            devices: Arc::new(DashMap::new()),
            scanning: Arc::new(Mutex::new(false)),
            advertising: Arc::new(AtomicBool::new(false)),
            notification_senders: Arc::new(DashMap::new()),
            event_senders: Arc::new(DashMap::new()),
            next_event_sender_id: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Add a simulated device to the monitor.
    ///
    /// # Arguments
    ///
    /// * `id` - The device ID to add
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::{DeviceMonitor, DeviceId};
    /// use bt_mon::backends::mock::MockMonitor;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let monitor = MockMonitor::new();
    /// let device_id = DeviceId::new("00:11:22:33:44:55");
    /// monitor.add_device(device_id.clone()).await?;
    ///
    /// let devices = monitor.devices().await?;
    /// assert_eq!(devices.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn add_device(&self, id: DeviceId) -> Result<()> {
        debug!("Adding simulated device: {}", id);

        let address = id.as_str().to_string();
        let device = BluetoothDevice::new(id.clone(), address)
            .with_name(format!("Mock Device {}", id))
            .with_rssi(self.config.default_rssi);

        let mut simulated_device = SimulatedDevice::new(device.clone());

        // Add default services if configured
        for service_config in &self.config.default_services {
            let service = self.service_from_config(service_config);
            simulated_device.services.push(service);
        }

        let id_clone = id.clone();
        self.devices.insert(id_clone, simulated_device);
        info!("Added simulated device: {}", id);
        self.broadcast_event(&DeviceEvent::DeviceAdded { device });
        Ok(())
    }

    /// Add a device with custom name and services.
    ///
    /// # Arguments
    ///
    /// * `id` - The device ID
    /// * `name` - The device name
    /// * `services` - The services to add to this device
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::{DeviceMonitor, DeviceId};
    /// use bt_mon::backends::mock::{MockMonitor, SimulatedService, SimulatedCharacteristic};
    /// use bt_mon::types::{ServiceUuid, CharacteristicUuid, CharacteristicProperties};
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let monitor = MockMonitor::new();
    /// let device_id = DeviceId::new("00:11:22:33:44:55");
    ///
    /// let service = SimulatedService::new(
    ///     ServiceUuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap(),
    ///     true,
    /// )
    /// .with_characteristic(SimulatedCharacteristic::new(
    ///     CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap(),
    ///     CharacteristicProperties::default().with_read(true),
    /// ).with_read_value(b"Device Name".to_vec()));
    ///
    /// monitor.add_device_with_services(device_id.clone(), "Custom Device", vec![service]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn add_device_with_services(
        &self,
        id: DeviceId,
        name: &str,
        services: Vec<SimulatedService>,
    ) -> Result<()> {
        debug!("Adding simulated device with services: {}", id);

        let address = id.as_str().to_string();
        let device = BluetoothDevice::new(id.clone(), address)
            .with_name(name)
            .with_rssi(self.config.default_rssi);

        let mut simulated_device = SimulatedDevice::new(device.clone());

        for service_config in &services {
            let service = self.service_from_config(service_config);
            simulated_device.services.push(service);
        }

        let id_clone = id.clone();
        self.devices.insert(id_clone, simulated_device);
        info!(
            "Added simulated device with {} services: {}",
            services.len(),
            id
        );
        self.broadcast_event(&DeviceEvent::DeviceAdded { device });
        Ok(())
    }

    /// Remove a simulated device from the monitor.
    ///
    /// # Arguments
    ///
    /// * `id` - The device ID to remove
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::{DeviceMonitor, DeviceId};
    /// use bt_mon::backends::mock::MockMonitor;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let monitor = MockMonitor::new();
    /// let device_id = DeviceId::new("00:11:22:33:44:55");
    ///
    /// monitor.add_device(device_id.clone()).await?;
    /// monitor.remove_device(&device_id).await;
    ///
    /// let devices = monitor.devices().await?;
    /// assert!(devices.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn remove_device(&self, id: &DeviceId) -> bool {
        debug!("Removing simulated device: {}", id);
        match self.devices.remove(id) {
            Some(_) => {
                self.broadcast_event(&DeviceEvent::DeviceRemoved { id: id.clone() });
                true
            }
            None => false,
        }
    }

    /// Clear all devices from the monitor.
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::{DeviceMonitor, DeviceId};
    /// use bt_mon::backends::mock::MockMonitor;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let monitor = MockMonitor::new();
    ///
    /// monitor.add_device(DeviceId::new("00:11:22:33:44:55")).await?;
    /// monitor.add_device(DeviceId::new("11:22:33:44:55:66")).await?;
    ///
    /// monitor.clear_devices().await;
    ///
    /// let devices = monitor.devices().await?;
    /// assert!(devices.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn clear_devices(&self) {
        debug!("Clearing all devices");
        let removed: Vec<DeviceId> = self.devices.iter().map(|e| e.key().clone()).collect();
        self.devices.clear();
        for id in removed {
            self.broadcast_event(&DeviceEvent::DeviceRemoved { id });
        }
    }

    /// Set simulated services for a device.
    ///
    /// # Arguments
    ///
    /// * `id` - The device ID
    /// * `services` - The services to set
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::{DeviceMonitor, DeviceId};
    /// use bt_mon::backends::mock::{MockMonitor, SimulatedService};
    /// use bt_mon::types::{ServiceUuid, CharacteristicProperties};
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let monitor = MockMonitor::new();
    /// let device_id = DeviceId::new("00:11:22:33:44:55");
    ///
    /// monitor.add_device(device_id.clone()).await?;
    ///
    /// let service = SimulatedService::new(
    ///     ServiceUuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap(),
    ///     true,
    /// );
    ///
    /// monitor.set_device_services(device_id.clone(), vec![service]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_device_services(
        &self,
        id: DeviceId,
        services: Vec<SimulatedService>,
    ) -> Result<()> {
        debug!("Setting services for device: {}", id);

        let mut device = self
            .devices
            .get_mut(&id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

        device.services.clear();
        for service_config in &services {
            let service = self.service_from_config(service_config);
            device.services.push(service);
        }

        info!("Set {} services for device: {}", services.len(), id);
        Ok(())
    }

    /// Convert a simulated service configuration to a GattService.
    fn service_from_config(&self, config: &SimulatedService) -> GattService {
        let characteristics = config
            .characteristics
            .iter()
            .map(|c| GattCharacteristic {
                uuid: c.uuid,
                properties: c.properties.clone(),
                handle: c.handle,
            })
            .collect();

        GattService {
            uuid: config.uuid,
            is_primary: config.is_primary,
            characteristics,
        }
    }

    /// Simulate a delay.
    async fn simulate_delay(&self, delay_ms: u64) {
        if delay_ms > 0 {
            time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    /// Create a notification event stream for a device.
    async fn create_notification_stream(
        &self,
        id: &DeviceId,
    ) -> tokio::sync::mpsc::Receiver<NotificationEvent> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        self.notification_senders.insert(id.clone(), tx);
        rx
    }

    /// Broadcast an event to all open device event streams.
    ///
    /// Senders whose receivers were dropped are removed from the registry.
    fn broadcast_event(&self, event: &DeviceEvent) {
        publish_event(&self.event_senders, event);
    }

    /// Puts the configured advertisers on the air until the scan stops.
    ///
    /// Each tick re-advertises every device, as a real radio does; the node's
    /// rate limiter is what decides whether a repeat sighting is worth a row.
    /// The device is registered with the monitor too, so `devices()` and the
    /// event stream agree.
    fn spawn_advertisers(&self) {
        let devices = self.devices.clone();
        let senders = self.event_senders.clone();
        let advertising = self.advertising.clone();
        let advertisers = self.config.advertisers.clone();
        let interval = Duration::from_millis(self.config.advertise_interval_ms.max(1));

        advertising.store(true, Ordering::SeqCst);
        tokio::spawn(async move {
            while advertising.load(Ordering::SeqCst) {
                for advertiser in &advertisers {
                    let device = BluetoothDevice::new(
                        advertiser.id.clone(),
                        advertiser.id.as_str().to_string(),
                    )
                    .with_name(advertiser.name.clone())
                    .with_rssi(advertiser.rssi)
                    .with_raw_payload(advertiser.advertisement_bytes());

                    // An existing entry is left alone: a scan must not wipe the
                    // services or connection state something else attached to the
                    // same address.
                    devices
                        .entry(advertiser.id.clone())
                        .or_insert_with(|| SimulatedDevice::new(device.clone()));

                    publish_event(&senders, &DeviceEvent::DeviceAdded { device });
                }

                time::sleep(interval).await;
            }
        });
    }
}

/// Delivers an event to every open subscriber, pruning the disconnected ones.
fn publish_event(
    senders: &DashMap<u64, futures::channel::mpsc::Sender<DeviceEvent>>,
    event: &DeviceEvent,
) {
    let mut dead = Vec::new();
    for mut entry in senders.iter_mut() {
        if let Err(err) = entry.value_mut().try_send(event.clone()) {
            if err.is_disconnected() {
                dead.push(*entry.key());
            } else {
                debug!("Mock device event channel full; dropping event");
            }
        }
    }
    for id in dead {
        senders.remove(&id);
    }
}

/// Stopping the advertiser loop on drop keeps a discarded monitor from
/// broadcasting forever: the task holds `Arc` clones, so dropping the monitor is
/// the only signal it has that nobody is listening any more.
impl Drop for MockMonitor {
    fn drop(&mut self) {
        self.advertising.store(false, Ordering::SeqCst);
    }
}

impl Default for MockMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DeviceMonitor for MockMonitor {
    async fn start_scan(&self) -> Result<()> {
        let mut scanning = self.scanning.lock().await;
        if *scanning {
            return Err(Error::ScanAlreadyInProgress);
        }

        debug!("Starting scan with mock backend");

        if self.config.error_on_scan {
            return Err(Error::InitFailed(self.config.scan_error_message.clone()));
        }

        self.simulate_delay(self.config.scan_delay_ms).await;

        *scanning = true;
        info!(
            "Scan started successfully on mock adapter '{}'",
            self.config.adapter_name
        );

        if !self.config.advertisers.is_empty() {
            self.spawn_advertisers();
        }

        Ok(())
    }

    async fn stop_scan(&self) -> Result<()> {
        let mut scanning = self.scanning.lock().await;
        if !*scanning {
            return Err(Error::NotScanning);
        }

        debug!("Stopping scan");
        *scanning = false;
        self.advertising.store(false, Ordering::SeqCst);
        info!("Scan stopped successfully");
        Ok(())
    }

    async fn devices(&self) -> Result<Vec<BluetoothDevice>> {
        Ok(self
            .devices
            .iter()
            .map(|e| e.value().device.clone())
            .collect())
    }

    async fn device(&self, id: &DeviceId) -> Result<BluetoothDevice> {
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        Ok(entry.value().device.clone())
    }

    async fn is_powered(&self) -> Result<bool> {
        Ok(self.config.adapter_powered)
    }

    async fn adapter_info(&self) -> Result<String> {
        // The name comes first, as it does on BlueZ ("<id> (<modalias>)"), so a
        // selector that would pick this adapter on a real radio picks it here
        // too — which is what makes the mock usable for checking adapter
        // selection at all.
        Ok(format!(
            "{} (mock) powered={}",
            self.config.adapter_name, self.config.adapter_powered
        ))
    }

    async fn device_events(&self) -> Result<DeviceEventStream> {
        let (mut tx, rx) = futures::channel::mpsc::channel(DEVICE_EVENT_CHANNEL_CAPACITY);

        // Replay the current devices so late subscribers see the existing
        // state (mirrors btleplug, which synthesizes initial DeviceDiscovered
        // events when the stream opens).
        let mut replay = Vec::new();
        for entry in self.devices.iter() {
            replay.push(DeviceEvent::DeviceAdded {
                device: entry.value().device.clone(),
            });
        }
        for event in replay {
            if tx.try_send(event).is_err() {
                // The receiver was already dropped; nothing else to do.
                return Ok(Box::pin(rx));
            }
        }

        let id = self.next_event_sender_id.fetch_add(1, Ordering::SeqCst);
        self.event_senders.insert(id, tx);

        // The stream stays open for the lifetime of the monitor; it only
        // completes when the monitor (and all its senders) are dropped.
        Ok(Box::pin(rx))
    }

    async fn is_scanning(&self) -> Result<bool> {
        let scanning = self.scanning.lock().await;
        Ok(*scanning)
    }
}

#[async_trait]
impl GattClient for MockMonitor {
    async fn connect(&self, id: &DeviceId) -> Result<()> {
        debug!("Connecting to mock device: {}", id);

        if self.config.error_on_connect {
            return Err(Error::InitFailed(self.config.connect_error_message.clone()));
        }

        // Existence is checked without keeping a guard: a `DashMap` read guard
        // held across `get_mut` below (or across the await) deadlocks on the
        // same shard, which is what this used to do — every test that connected
        // to a device hung forever.
        if !self.devices.contains_key(id) {
            return Err(Error::DeviceNotFound(id.clone()));
        }

        self.simulate_delay(self.config.connect_delay_ms).await;

        // Update connection status
        if let Some(mut entry) = self.devices.get_mut(id) {
            entry.value_mut().device.is_connected = true;
        }

        info!("Connected to mock device: {}", id);
        Ok(())
    }

    async fn disconnect(&self, id: &DeviceId) -> Result<()> {
        debug!("Disconnecting from mock device: {}", id);

        if let Some(mut entry) = self.devices.get_mut(id) {
            entry.value_mut().device.is_connected = false;
        }

        info!("Disconnected from mock device: {}", id);
        Ok(())
    }

    async fn is_connected(&self, id: &DeviceId) -> Result<bool> {
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        Ok(entry.value().device.is_connected)
    }

    async fn discover_services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        debug!("Discovering services for mock device: {}", id);

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

        let is_connected = entry.value().device.is_connected;
        drop(entry);

        if !is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        let entry = self.devices.get(id).unwrap();
        let services = entry.value().services.clone();
        drop(entry);

        info!(
            "Discovered {} services for mock device: {}",
            services.len(),
            id
        );
        Ok(services)
    }

    async fn services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        Ok(entry.value().services.clone())
    }

    async fn read_characteristic(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
    ) -> Result<Vec<u8>> {
        debug!("Reading characteristic {} from mock device {}", uuid, id);

        if self.config.error_on_read {
            return Err(Error::Internal(self.config.read_error_message.clone()));
        }

        self.simulate_delay(self.config.read_delay_ms).await;

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

        let is_connected = entry.value().device.is_connected;

        // Search through services for the characteristic
        let mut allow_read = true;

        for service in &entry.value().services {
            for char in &service.characteristics {
                if char.uuid == *uuid {
                    allow_read = char.can_read();
                    // We don't store read_value here since GattCharacteristic is the public type
                    // Users should configure their mock to return expected values via error simulation
                    break;
                }
            }
        }

        drop(entry);

        if !is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        if !allow_read {
            return Err(Error::CharacteristicNotFound(*uuid));
        }

        // Return empty vector for characteristics without configured values
        // Users can configure error_on_read to test error handling
        debug!("Read {} bytes from mock characteristic {}", 0, uuid);
        Ok(Vec::new())
    }

    async fn write_characteristic(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
        value: &[u8],
        _response: bool,
    ) -> Result<()> {
        debug!(
            "Writing to characteristic {} on mock device {} ({} bytes)",
            uuid,
            id,
            value.len()
        );

        if self.config.error_on_write {
            return Err(Error::Internal(self.config.write_error_message.clone()));
        }

        self.simulate_delay(self.config.write_delay_ms).await;

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

        let is_connected = entry.value().device.is_connected;
        drop(entry);

        if !is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        debug!(
            "Successfully wrote {} bytes to mock characteristic {}",
            value.len(),
            uuid
        );
        Ok(())
    }

    async fn subscribe(&self, id: &DeviceId, uuid: &CharacteristicUuid) -> Result<()> {
        debug!(
            "Subscribing to notifications for characteristic {} on mock device {}",
            uuid, id
        );

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

        let is_connected = entry.value().device.is_connected;

        if !is_connected {
            drop(entry);
            return Err(Error::NotConnected(id.clone()));
        }

        // Mark as subscribed (drop entry before acquiring write lock)
        drop(entry);
        if let Some(entry) = self.devices.get(id) {
            let mut subscribed = entry.value().subscribed_characteristics.write().await;
            subscribed.insert(*uuid, true);
        }

        debug!("Successfully subscribed to characteristic {}", uuid);
        Ok(())
    }

    async fn unsubscribe(&self, id: &DeviceId, uuid: &CharacteristicUuid) -> Result<()> {
        debug!(
            "Unsubscribing from characteristic {} on mock device {}",
            uuid, id
        );

        if let Some(entry) = self.devices.get(id) {
            let mut subscribed = entry.value().subscribed_characteristics.write().await;
            subscribed.remove(uuid);
        }

        debug!("Successfully unsubscribed from characteristic {}", uuid);
        Ok(())
    }

    async fn notifications(&self, id: &DeviceId) -> Result<NotificationStream> {
        debug!("Setting up notification stream for mock device {}", id);

        let rx = self.create_notification_stream(id).await;
        // Convert NotificationEvent stream to ValueNotification stream
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event.notification, rx))
        });

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{Stream, StreamExt};
    use std::pin::Pin;

    #[tokio::test]
    async fn test_mock_monitor_creation() {
        let monitor = MockMonitor::new();
        assert!(monitor.is_powered().await.unwrap());
        assert!(!monitor.is_scanning().await.unwrap());
    }

    #[test]
    fn an_advertisers_payload_is_its_own_and_the_same_every_time() {
        let beacon = SimulatedAdvertiser::new("F0:EE:00:00:01:02", "Beacon A");
        let other = SimulatedAdvertiser::new("F0:EE:00:00:01:03", "Beacon A");

        assert_eq!(
            beacon.advertisement_bytes(),
            beacon.advertisement_bytes(),
            "a stored payload has to be comparable across runs"
        );
        assert_ne!(
            beacon.advertisement_bytes(),
            other.advertisement_bytes(),
            "two beacons must not look like one device"
        );
    }

    #[test]
    fn an_identifier_that_is_not_a_mac_still_gets_six_address_bytes() {
        // The mock accepts any string as an identifier; the manufacturer block
        // is always six bytes whatever arrives.
        assert_eq!(address_bytes("not-a-mac").len(), 6);
        assert_eq!(
            address_bytes("AA:BB:CC:DD:EE:FF"),
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]
        );
    }

    #[tokio::test]
    async fn test_mock_monitor_with_config() {
        let config = MockConfig::default()
            .with_adapter_name("TestAdapter")
            .with_default_rssi(-70)
            .with_error_on_scan(true);

        let monitor = MockMonitor::with_config(config);
        assert_eq!(
            monitor.adapter_info().await.unwrap(),
            "TestAdapter (mock) powered=true"
        );
        assert!(monitor.is_powered().await.unwrap());
    }

    #[tokio::test]
    async fn test_add_device() {
        let monitor = MockMonitor::new();
        let device_id = DeviceId::new("00:11:22:33:44:55");

        monitor.add_device(device_id.clone()).await.unwrap();

        let devices = monitor.devices().await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, device_id);
        assert_eq!(
            devices[0].name,
            Some("Mock Device 00:11:22:33:44:55".to_string())
        );
    }

    #[tokio::test]
    async fn test_remove_device() {
        let monitor = MockMonitor::new();
        let device_id = DeviceId::new("00:11:22:33:44:55");

        monitor.add_device(device_id.clone()).await.unwrap();
        monitor.remove_device(&device_id).await;

        let devices = monitor.devices().await.unwrap();
        assert!(devices.is_empty());
    }

    #[tokio::test]
    async fn test_clear_devices() {
        let monitor = MockMonitor::new();

        monitor
            .add_device(DeviceId::new("00:11:22:33:44:55"))
            .await
            .unwrap();
        monitor
            .add_device(DeviceId::new("11:22:33:44:55:66"))
            .await
            .unwrap();

        monitor.clear_devices().await;

        let devices = monitor.devices().await.unwrap();
        assert!(devices.is_empty());
    }

    #[tokio::test]
    async fn test_start_stop_scan() {
        let monitor = MockMonitor::new();

        monitor.start_scan().await.unwrap();
        assert!(monitor.is_scanning().await.unwrap());

        monitor.stop_scan().await.unwrap();
        assert!(!monitor.is_scanning().await.unwrap());
    }

    #[tokio::test]
    async fn test_scan_already_in_progress() {
        let monitor = MockMonitor::new();

        monitor.start_scan().await.unwrap();
        let result = monitor.start_scan().await;
        assert!(matches!(result, Err(Error::ScanAlreadyInProgress)));
    }

    #[tokio::test]
    async fn test_not_scanning_error() {
        let monitor = MockMonitor::new();

        let result = monitor.stop_scan().await;
        assert!(matches!(result, Err(Error::NotScanning)));
    }

    #[tokio::test]
    async fn test_scan_error_simulation() {
        let config = MockConfig::default().with_error_on_scan(true);
        let monitor = MockMonitor::with_config(config);

        let result = monitor.start_scan().await;
        assert!(matches!(result, Err(Error::InitFailed(_))));
    }

    #[tokio::test]
    async fn test_connect_disconnect() {
        let monitor = MockMonitor::new();
        let device_id = DeviceId::new("00:11:22:33:44:55");

        monitor.add_device(device_id.clone()).await.unwrap();

        // Set very short delays to speed up test
        let config = MockConfig::default().with_connect_delay_ms(1);
        let monitor_fast = MockMonitor::with_config(config);
        monitor_fast.add_device(device_id.clone()).await.unwrap();

        monitor_fast.connect(&device_id).await.unwrap();
        assert!(monitor_fast.is_connected(&device_id).await.unwrap());

        monitor_fast.disconnect(&device_id).await.unwrap();
        assert!(!monitor_fast.is_connected(&device_id).await.unwrap());
    }

    #[tokio::test]
    async fn test_connect_error_simulation() {
        let config = MockConfig::default().with_error_on_connect(true);
        let monitor = MockMonitor::with_config(config);

        let device_id = DeviceId::new("00:11:22:33:44:55");
        monitor.add_device(device_id.clone()).await.unwrap();

        let result = monitor.connect(&device_id).await;
        assert!(matches!(result, Err(Error::InitFailed(_))));
    }

    #[tokio::test]
    async fn test_read_characteristic() {
        let config = MockConfig::default()
            .with_connect_delay_ms(1)
            .with_read_delay_ms(1);
        let monitor = MockMonitor::with_config(config);
        let device_id = DeviceId::new("00:11:22:33:44:55");

        monitor.add_device(device_id.clone()).await.unwrap();
        monitor.connect(&device_id).await.unwrap();

        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();

        // Read should return empty vector for unconfigured characteristics
        let value = monitor
            .read_characteristic(&device_id, &char_uuid)
            .await
            .unwrap();
        assert!(value.is_empty());
    }

    #[tokio::test]
    async fn test_read_error_simulation() {
        let config = MockConfig::default()
            .with_error_on_read(true)
            .with_connect_delay_ms(1);
        let monitor = MockMonitor::with_config(config);

        let device_id = DeviceId::new("00:11:22:33:44:55");
        monitor.add_device(device_id.clone()).await.unwrap();
        monitor.connect(&device_id).await.unwrap();

        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();

        let result = monitor.read_characteristic(&device_id, &char_uuid).await;
        assert!(matches!(result, Err(Error::Internal(_))));
    }

    #[tokio::test]
    async fn test_write_characteristic() {
        let config = MockConfig::default()
            .with_connect_delay_ms(1)
            .with_write_delay_ms(1);
        let monitor = MockMonitor::with_config(config);
        let device_id = DeviceId::new("00:11:22:33:44:55");

        monitor.add_device(device_id.clone()).await.unwrap();
        monitor.connect(&device_id).await.unwrap();

        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();

        let value = vec![0x01, 0x02, 0x03, 0x04];
        monitor
            .write_characteristic(&device_id, &char_uuid, &value, true)
            .await
            .unwrap();

        // Write should succeed without error
    }

    #[tokio::test]
    async fn test_write_error_simulation() {
        let config = MockConfig::default()
            .with_error_on_write(true)
            .with_connect_delay_ms(1);
        let monitor = MockMonitor::with_config(config);

        let device_id = DeviceId::new("00:11:22:33:44:55");
        monitor.add_device(device_id.clone()).await.unwrap();
        monitor.connect(&device_id).await.unwrap();

        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();

        let value = vec![0x01, 0x02, 0x03, 0x04];
        let result = monitor
            .write_characteristic(&device_id, &char_uuid, &value, true)
            .await;
        assert!(matches!(result, Err(Error::Internal(_))));
    }

    #[tokio::test]
    async fn test_subscribe_unsubscribe() {
        let config = MockConfig::default().with_connect_delay_ms(1);
        let monitor = MockMonitor::with_config(config);
        let device_id = DeviceId::new("00:11:22:33:44:55");

        monitor.add_device(device_id.clone()).await.unwrap();
        monitor.connect(&device_id).await.unwrap();

        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();

        monitor.subscribe(&device_id, &char_uuid).await.unwrap();
        monitor.unsubscribe(&device_id, &char_uuid).await.unwrap();
    }

    #[tokio::test]
    async fn test_device_not_found() {
        let monitor = MockMonitor::new();
        let device_id = DeviceId::new("00:11:22:33:44:55");

        let result = monitor.device(&device_id).await;
        assert!(matches!(result, Err(Error::DeviceNotFound(_))));
    }

    #[tokio::test]
    async fn test_not_connected_error() {
        let monitor = MockMonitor::new();
        let device_id = DeviceId::new("00:11:22:33:44:55");

        monitor.add_device(device_id.clone()).await.unwrap();

        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();

        let result = monitor.read_characteristic(&device_id, &char_uuid).await;
        assert!(matches!(result, Err(Error::NotConnected(_))));
    }

    #[tokio::test]
    async fn test_config_builder_pattern() {
        let config = MockConfig::default()
            .with_scan_delay_ms(500)
            .with_read_delay_ms(100)
            .with_write_delay_ms(100)
            .with_connect_delay_ms(300)
            .with_error_on_scan(false)
            .with_error_on_connect(false)
            .with_error_on_read(false)
            .with_error_on_write(false)
            .with_default_rssi(-80)
            .with_adapter_powered(true)
            .with_adapter_name("CustomAdapter");

        assert_eq!(config.scan_delay_ms, 500);
        assert_eq!(config.read_delay_ms, 100);
        assert_eq!(config.default_rssi, -80);
        assert_eq!(config.adapter_name, "CustomAdapter");
        assert!(config.adapter_powered);
    }

    #[tokio::test]
    async fn test_simulated_service_and_characteristic() {
        let service_uuid = ServiceUuid::parse_str("00001800-0000-1000-8000-00805f9b34fb").unwrap();
        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();

        let service = SimulatedService::new(service_uuid, true).with_characteristic(
            SimulatedCharacteristic::new(
                char_uuid,
                CharacteristicProperties::new().with_read(true),
            )
            .with_handle(0x0010)
            .with_read_value(b"Test Value".to_vec())
            .with_allow_read(true),
        );

        assert_eq!(service.uuid, service_uuid);
        assert!(service.is_primary);
        assert_eq!(service.characteristics.len(), 1);
        assert_eq!(service.characteristics[0].handle, Some(0x0010));
    }

    #[tokio::test]
    async fn test_multiple_devices() {
        let monitor = MockMonitor::new();

        for i in 0..5 {
            let device_id = DeviceId::new(format!(
                "{:02}:{:02}:{:02}:{:02}:{:02}:{:02}",
                0, 0, 0, 0, 0, i
            ));
            monitor.add_device(device_id).await.unwrap();
        }

        let devices = monitor.devices().await.unwrap();
        assert_eq!(devices.len(), 5);
    }

    #[tokio::test]
    async fn test_adapter_info() {
        let config = MockConfig::default().with_adapter_name("TestAdapter");
        let monitor = MockMonitor::with_config(config);

        let info = monitor.adapter_info().await.unwrap();
        assert!(info.contains("TestAdapter"));
        assert!(info.contains("powered=true"));
    }

    #[tokio::test]
    async fn test_mock_unpowered_adapter() {
        let config = MockConfig::default().with_adapter_powered(false);
        let monitor = MockMonitor::with_config(config);

        assert!(!monitor.is_powered().await.unwrap());
    }

    async fn next_event(
        stream: &mut Pin<Box<dyn Stream<Item = DeviceEvent> + Send>>,
        timeout: Duration,
    ) -> Option<DeviceEvent> {
        tokio::time::timeout(timeout, stream.next())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    async fn test_device_events_replays_existing_devices() {
        let monitor = MockMonitor::new();
        monitor
            .add_device(DeviceId::new("00:11:22:33:44:55"))
            .await
            .unwrap();
        monitor
            .add_device(DeviceId::new("11:22:33:44:55:66"))
            .await
            .unwrap();

        let mut events = monitor.device_events().await.unwrap();

        let first = next_event(&mut events, Duration::from_secs(2))
            .await
            .expect("expected replayed DeviceAdded");
        assert!(matches!(first, DeviceEvent::DeviceAdded { .. }));

        let second = next_event(&mut events, Duration::from_secs(2))
            .await
            .expect("expected replayed DeviceAdded");
        assert!(matches!(second, DeviceEvent::DeviceAdded { .. }));
    }

    #[tokio::test]
    async fn test_device_events_broadcasts_add_and_remove() {
        let monitor = MockMonitor::new();
        let mut events = monitor.device_events().await.unwrap();

        let device_id = DeviceId::new("00:11:22:33:44:55");
        monitor.add_device(device_id.clone()).await.unwrap();

        let event = next_event(&mut events, Duration::from_secs(2))
            .await
            .expect("expected DeviceAdded");
        match event {
            DeviceEvent::DeviceAdded { device } => assert_eq!(device.id, device_id),
            _ => panic!("expected DeviceAdded"),
        }

        monitor.remove_device(&device_id).await;

        let event = next_event(&mut events, Duration::from_secs(2))
            .await
            .expect("expected DeviceRemoved");
        match event {
            DeviceEvent::DeviceRemoved { id } => assert_eq!(id, device_id),
            _ => panic!("expected DeviceRemoved"),
        }
    }

    #[tokio::test]
    async fn test_device_events_stays_open_while_monitor_lives() {
        let monitor = MockMonitor::new();
        let mut events = monitor.device_events().await.unwrap();

        // No devices means no replay, so a bounded wait should time out
        // rather than the stream completing.
        let result = tokio::time::timeout(Duration::from_millis(200), events.next()).await;
        assert!(result.is_err(), "stream completed while monitor is alive");
    }

    #[tokio::test]
    async fn test_configured_advertisers_reach_an_open_stream_while_scanning() {
        let monitor = MockMonitor::with_config(
            MockConfig::default()
                .with_advertiser_count(3)
                .with_advertise_interval_ms(20),
        );
        let mut events = monitor.device_events().await.unwrap();

        monitor.start_scan().await.unwrap();

        let mut seen: Vec<DeviceId> = Vec::new();
        while seen.len() < 3 {
            let event = next_event(&mut events, Duration::from_secs(2))
                .await
                .expect("an advertiser should be heard from while scanning");
            if let DeviceEvent::DeviceAdded { device } = event {
                if !seen.contains(&device.id) {
                    seen.push(device.id);
                }
            }
        }

        // The stream and `devices()` agree, so a caller that lists rather than
        // listens still sees the simulated room.
        assert_eq!(monitor.devices().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_a_monitor_without_advertisers_stays_quiet() {
        // The default has to stay an empty room: every unit test here adds its
        // own devices and would have its assertions polluted by beacons that
        // advertise themselves.
        let monitor = MockMonitor::new();
        let mut events = monitor.device_events().await.unwrap();

        monitor.start_scan().await.unwrap();

        assert!(
            tokio::time::timeout(Duration::from_millis(200), events.next())
                .await
                .is_err(),
            "a monitor without advertisers should advertise nothing"
        );
    }
}
