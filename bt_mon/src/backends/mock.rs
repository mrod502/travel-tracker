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
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time;

use crate::config::MonitorConfig;
use crate::error::{BackendKind, Error, Result};
use crate::monitor::events::{
    changed_device_fields, report_event, DeviceEvent, DeviceEventSinks, DeviceEventStream,
    NotificationHub, NotificationStream,
};
use crate::monitor::health::MonitorHealth;
use crate::monitor::{DeviceMonitor, GattClient};
use crate::types::{
    ad_structure, BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId,
    GattCharacteristic, GattService, ServiceUuid, ValueNotification,
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
    /// Values a subscribed characteristic emits, in order, cycling.
    ///
    /// Only used when [`auto_send_notifications`](Self::auto_send_notifications) is
    /// on; a characteristic with no entry here stays quiet. A test that wants to say
    /// one specific thing at one specific moment calls
    /// [`MockMonitor::send_notification`] instead.
    pub notification_values: HashMap<CharacteristicUuid, Vec<Vec<u8>>>,
    /// Whether a subscribed characteristic starts emitting
    /// [`notification_values`](Self::notification_values) on its own.
    ///
    /// Off by default, so a test that subscribes sees only what it injects. On, each
    /// successful `subscribe()` for a characteristic that has values starts a task
    /// that replays them on [`notification_interval_ms`](Self::notification_interval_ms)
    /// until the characteristic is unsubscribed or the monitor is dropped.
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
    ///
    /// `0` is not "as fast as possible" — it is "not configured", and the simulated
    /// radio advertises on the default cadence instead. A monitor built from a
    /// [`MonitorConfig`] whose scan interval is zero lands here, and a zero scan
    /// interval on a real radio means "arm once, do not churn the controller", not
    /// "stop reporting advertisements".
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
    ///
    /// A simulated advertiser *is* the radio here, so its payload is labelled
    /// [`RawPayloadSource::Captured`](crate::types::RawPayloadSource::Captured):
    /// unlike a `bluer` payload, what is not in these bytes was not advertised.
    pub fn advertisement_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        // Flags: LE General Discoverable, no BR/EDR re-support.
        bytes.extend(ad_structure(0x01, &[0x06]).expect("one flag byte fits"));
        // Complete local name (AD forbids more than 30 bytes of payload).
        let name = self.name.as_bytes();
        bytes.extend(
            ad_structure(0x09, &name[..name.len().min(30)])
                .expect("a name capped at 30 bytes fits one structure"),
        );
        // Manufacturer-specific data: a reserved company id plus the address
        // bytes, which is what makes each beacon's payload distinct.
        let mut manufacturer = 0xFFFFu16.to_le_bytes().to_vec();
        manufacturer.extend_from_slice(&address_bytes(self.id.as_str()));
        bytes.extend(ad_structure(0xFF, &manufacturer).expect("8 bytes fit one structure"));
        bytes
    }
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

/// The simulated radio `config` asks for.
///
/// Every consumer that can run against a mock used to re-implement this translation
/// at its own call site — `app` did, interval and adapter name both — which means a
/// consumer that forgot got a mock ignoring its configuration while the test believed
/// it had set something. Doing it here is the point.
///
/// The two settings mean:
///
/// * `adapter` becomes the adapter's *name*, not a selector matched against one. The
///   mock has exactly one radio, and the useful behaviour is that `adapter_info()`
///   reports the name the operator asked for, so `--adapter-id` stays observable end
///   to end. No selector means the default name.
/// * `scan_interval` becomes the advertisement cadence — see
///   [`MockConfig::advertise_interval_ms`] for what zero and an absurdly large interval mean.
///
/// Everything else keeps the [`Default`] shape: a caller adds advertisers and
/// simulation switches on top, as in
/// `MockConfig::from(&monitor_config).with_advertiser_count(3)`.
impl From<&MonitorConfig> for MockConfig {
    fn from(config: &MonitorConfig) -> Self {
        let mut mock = Self::default();

        if let Some(selector) = config.adapter.as_deref() {
            mock.adapter_name = selector.to_string();
        }

        mock.advertise_interval_ms = advertise_interval_ms(config.scan_interval);
        mock
    }
}

/// Milliseconds for a scan interval, in the units [`MockConfig::advertise_interval_ms`]
/// counts in, with both edge cases defined rather than accidental:
///
/// * **Zero** passes through, and a cadence of zero means "not configured": the radio
///   advertises on the default cadence. A zero scan interval on a real backend means
///   "arm the controller once, do not re-arm it", and the radio keeps reporting
///   advertisements all the same; treating zero as silence would make the mock
///   disagree with the thing it stands in for.
/// * **Too large for a `u64`** saturates to `u64::MAX`, which means one advertisement
///   per device per scan and then no further tick for longer than the sun. That is
///   the defined meaning of an interval this large, not a truncation of it: an
///   operator who asks the radio to re-arm every 584 million years gets one report.
fn advertise_interval_ms(scan_interval: Duration) -> u64 {
    u64::try_from(scan_interval.as_millis()).unwrap_or(u64::MAX)
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
///
/// Every field here changes how the monitor behaves, which is the contract this type
/// has to keep: a simulated field that goes nowhere is a test that passes for a
/// reason that does not exist.
///
/// * [`properties`](Self::properties) — what the characteristic advertises it can do;
///   read, write and subscribe all check it.
/// * [`read_value`](Self::read_value) — the bytes `read_characteristic` returns until
///   something writes over them. A write followed by a read returns what was written.
/// * [`allow_read`](Self::allow_read) / [`allow_write`](Self::allow_write) /
///   [`allow_subscribe`](Self::allow_subscribe) — a permission the *simulated device*
///   enforces, on top of `properties`. Set one false and the call fails with
///   [`Error::InvalidArgument`], which is what a real device answers when asked to
///   read something it does not expose for reading.
///
/// A characteristic is addressed by uuid alone, exactly as the trait addresses it, so
/// giving the same uuid to two characteristics on one device makes them one value
/// store. That is the API's limitation, not the mock's.
#[derive(Debug, Clone)]
pub struct SimulatedCharacteristic {
    /// Characteristic UUID.
    pub uuid: CharacteristicUuid,
    /// Characteristic properties.
    pub properties: CharacteristicProperties,
    /// Simulated handle.
    pub handle: Option<u16>,
    /// The bytes this characteristic reads as, until something writes to it.
    pub read_value: Vec<u8>,
    /// Whether read is allowed. `false` makes `read_characteristic` fail.
    pub allow_read: bool,
    /// Whether write is allowed. `false` makes `write_characteristic` fail.
    pub allow_write: bool,
    /// Whether notifications can be subscribed. `false` makes `subscribe` fail.
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

    /// Whether these properties make a subscription honest.
    ///
    /// `allow_subscribe` is the harness's veto on top of this: a characteristic that
    /// declares neither notify nor indicate has nothing to deliver, and `subscribe`
    /// says so instead of opening a stream that stays silent forever.
    fn notifies(&self) -> bool {
        self.properties.notify || self.properties.indicate
    }
}

/// Internal representation of a simulated device.
///
/// Holds the *simulation spec* — `SimulatedService`, with its permissions and read
/// values — rather than the converted [`GattService`]s, because the conversion drops
/// precisely what the GATT calls need in order to enforce anything. It used to store
/// the converted form, and that is why `read_characteristic` could only ever hand back
/// an empty `Vec` and why `allow_read(false)` changed nothing.
#[derive(Clone)]
struct SimulatedDevice {
    device: BluetoothDevice,
    services: Vec<SimulatedService>,
    /// Characteristic uuid → the bytes it currently reads as. Seeded from
    /// `SimulatedCharacteristic::read_value`, overwritten by a write.
    values: HashMap<CharacteristicUuid, Vec<u8>>,
    /// Characteristics with an open subscription.
    subscribed: HashSet<CharacteristicUuid>,
}

impl SimulatedDevice {
    /// Create a new simulated device.
    fn new(device: BluetoothDevice) -> Self {
        Self {
            device,
            services: Vec::new(),
            values: HashMap::new(),
            subscribed: HashSet::new(),
        }
    }

    /// Seeds the value store from the configured read values.
    fn seed_values(&mut self) {
        for service in &self.services {
            for characteristic in &service.characteristics {
                self.values
                    .insert(characteristic.uuid, characteristic.read_value.clone());
            }
        }
    }
}

/// The characteristic specification for `uuid`, wherever it sits in the table.
fn find_characteristic<'a>(
    services: &'a [SimulatedService],
    uuid: &CharacteristicUuid,
) -> Option<&'a SimulatedCharacteristic> {
    services
        .iter()
        .flat_map(|service| service.characteristics.iter())
        .find(|characteristic| characteristic.uuid == *uuid)
}

/// The GATT table a caller sees, converted from the simulation spec.
fn public_services(services: &[SimulatedService]) -> Vec<GattService> {
    services.iter().map(GattService::from).collect()
}

impl From<&SimulatedCharacteristic> for GattCharacteristic {
    fn from(spec: &SimulatedCharacteristic) -> Self {
        Self {
            uuid: spec.uuid,
            properties: spec.properties.clone(),
            handle: spec.handle,
        }
    }
}

impl From<&SimulatedService> for GattService {
    fn from(spec: &SimulatedService) -> Self {
        Self {
            uuid: spec.uuid,
            is_primary: spec.is_primary,
            characteristics: spec
                .characteristics
                .iter()
                .map(GattCharacteristic::from)
                .collect(),
        }
    }
}

/// The service that contains `uuid`, so a notification frame can name it.
fn service_of(services: &[SimulatedService], uuid: &CharacteristicUuid) -> Option<ServiceUuid> {
    services
        .iter()
        .find(|service| {
            service
                .characteristics
                .iter()
                .any(|characteristic| characteristic.uuid == *uuid)
        })
        .map(|service| service.uuid)
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
    /// Bumped by every `start_scan` so the loop belonging to an earlier scan exits,
    /// rather than one looping advertiser accumulating per scan restart. A
    /// `stop_scan` → `start_scan` pair inside one `advertise_interval_ms` never lets
    /// the old loop observe `advertising` go false, which is why a flag alone is not
    /// enough to stop it.
    scan_generation: Arc<AtomicU64>,
    /// Notification fan-out: one channel per device, every consumer of that device
    /// getting every frame.
    notifications: NotificationHub,
    /// The `auto_send_notifications` loops, one per (device, characteristic), so
    /// `unsubscribe` and drop can end them.
    notification_tasks: Arc<DashMap<(DeviceId, CharacteristicUuid), JoinHandle<()>>>,
    /// Device event fan-out, shared with anything that publishes — the advertiser
    /// loop, a locally-initiated connect, a device added by hand.
    events: DeviceEventSinks,
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
            scan_generation: Arc::new(AtomicU64::new(0)),
            notifications: NotificationHub::new(),
            notification_tasks: Arc::new(DashMap::new()),
            events: DeviceEventSinks::new(),
        }
    }

    /// Create a mock monitor for the adapter and cadence `config` selects.
    ///
    /// The mock counterpart of [`create_btleplug_monitor_with_config`](crate::create_btleplug_monitor_with_config):
    /// a consumer that honours its configuration on the real radio and translates it
    /// by hand for the mock has two places to get it wrong. Use
    /// [`MockConfig::from`] when the simulation knobs (advertisers, injected errors)
    /// also matter, since this takes the defaults for those.
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::backends::mock::MockMonitor;
    /// use bt_mon::{DeviceMonitor, MonitorConfig};
    /// use std::time::Duration;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let monitor = MockMonitor::with_monitor_config(
    ///     &MonitorConfig::new()
    ///         .with_adapter("hci1")
    ///         .with_scan_interval(Duration::from_millis(250)),
    /// );
    ///
    /// // The selector the operator wrote is the adapter the monitor reports, which is
    /// // what makes a mock substituted for a real backend answer the same questions.
    /// assert!(monitor.adapter_info().await?.contains("hci1"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_monitor_config(config: &MonitorConfig) -> Self {
        Self::with_config(MockConfig::from(config))
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

        // Add default services if configured. The spec is kept as configured — the
        // GATT calls enforce permissions and serve read values out of it.
        for service_config in &self.config.default_services {
            simulated_device.services.push(service_config.clone());
        }
        simulated_device.seed_values();

        let id_clone = id.clone();
        self.devices.insert(id_clone, simulated_device);
        info!("Added simulated device: {}", id);
        self.publish(DeviceEvent::DeviceAdded { device });
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
            simulated_device.services.push(service_config.clone());
        }
        simulated_device.seed_values();

        let id_clone = id.clone();
        self.devices.insert(id_clone, simulated_device);
        info!(
            "Added simulated device with {} services: {}",
            services.len(),
            id
        );
        self.publish(DeviceEvent::DeviceAdded { device });
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
                self.forget_device(id);
                self.publish(DeviceEvent::DeviceRemoved { id: id.clone() });
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
            self.forget_device(&id);
            self.publish(DeviceEvent::DeviceRemoved { id });
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

        device.services = services;
        // The value store follows the new table: a characteristic that is gone stops
        // reading as something, and a new one reads as its configured value.
        device.values.clear();
        device.seed_values();
        device.subscribed.clear();

        info!("Set {} services for device: {}", device.services.len(), id);
        Ok(())
    }

    /// Simulate a delay.
    async fn simulate_delay(&self, delay_ms: u64) {
        if delay_ms > 0 {
            time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    /// Delivers an event to every open stream of this monitor.
    fn publish(&self, event: DeviceEvent) {
        self.events.publish(&event);
    }

    /// Applies `change` to a device's stored snapshot, returning the snapshots from
    /// either side of it when the stored state actually moved.
    ///
    /// The `None` case matters as much as the `Some`: BlueZ emits a `PropertiesChanged`
    /// only for a property that moved, so `connect()` on a device that is already
    /// connected must not invent a change to report. Diffing through
    /// [`changed_device_fields`] rather than comparing by hand is what keeps the mock
    /// agreeing with the real backends about what counts as a change.
    fn update_device(
        &self,
        id: &DeviceId,
        change: impl FnOnce(&mut BluetoothDevice),
    ) -> Option<(BluetoothDevice, BluetoothDevice)> {
        let mut entry = self.devices.get_mut(id)?;
        let previous = entry.value().device.clone();
        change(&mut entry.value_mut().device);
        let reported = entry.value().device.clone();
        let changed = !changed_device_fields(&previous, &reported).is_empty();
        changed.then_some((previous, reported))
    }

    /// Puts a transition a GATT call just caused on the event stream.
    fn report_change(&self, change: Option<(BluetoothDevice, BluetoothDevice)>) {
        let Some((previous, reported)) = change else {
            return;
        };
        self.publish(report_event(&previous, reported));
    }

    /// Ends what a device's presence kept alive: its notification streams and the
    /// auto-send loops serving it.
    fn forget_device(&self, id: &DeviceId) {
        let keys: Vec<(DeviceId, CharacteristicUuid)> = self
            .notification_tasks
            .iter()
            .filter(|entry| entry.key().0 == *id)
            .map(|entry| entry.key().clone())
            .collect();

        for key in keys {
            if let Some((_, task)) = self.notification_tasks.remove(&key) {
                task.abort();
            }
        }

        self.notifications.forget(id);
    }

    /// Puts a notification on the air, as the characteristic itself would.
    ///
    /// This is the harness's own hand on the radio: a test says what changed and when,
    /// instead of hoping a simulated loop says it for it. The frame goes to **every**
    /// open stream of that device — two callers of `notifications()` both receive it,
    /// and neither one closes the other — and carries the service the characteristic
    /// sits in, when the device's GATT table says which that is.
    ///
    /// Returns how many streams received it. `0` is not an error: a device can notify
    /// while nothing is reading.
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::backends::mock::{MockMonitor, SimulatedCharacteristic, SimulatedService};
    /// use bt_mon::{
    ///     CharacteristicProperties, CharacteristicUuid, DeviceId, GattClient, ServiceUuid,
    /// };
    /// use futures::StreamExt;
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), bt_mon::Error> {
    /// let monitor = MockMonitor::new();
    /// let device_id = DeviceId::new("AA:BB:CC:DD:EE:FF");
    /// let service = ServiceUuid::parse_str("0000180d-0000-1000-8000-00805f9b34fb")?;
    /// let characteristic = CharacteristicUuid::parse_str("00002a37-0000-1000-8000-00805f9b34fb")?;
    ///
    /// monitor
    ///     .add_device_with_services(
    ///         device_id.clone(),
    ///         "Heart Rate Belt",
    ///         vec![SimulatedService::new(service, true).with_characteristic(
    ///             SimulatedCharacteristic::new(
    ///                 characteristic,
    ///                 CharacteristicProperties::new().with_notify(true),
    ///             ),
    ///         )],
    ///     )
    ///     .await?;
    /// monitor.connect(&device_id).await?;
    /// monitor.subscribe(&device_id, &characteristic).await?;
    ///
    /// let mut frames = monitor.notifications(&device_id).await?;
    /// monitor.send_notification(&device_id, &characteristic, vec![0, 62])?;
    ///
    /// let frame = frames.next().await.expect("the injected frame");
    /// assert_eq!(frame.as_slice(), &[0, 62]);
    /// assert_eq!(frame.service, Some(service));
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_notification(
        &self,
        id: &DeviceId,
        characteristic: &CharacteristicUuid,
        value: Vec<u8>,
    ) -> Result<usize> {
        let service = {
            let entry = self
                .devices
                .get(id)
                .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
            service_of(&entry.services, characteristic)
        };

        let notification = ValueNotification::new(*characteristic, value);
        let notification = match service {
            Some(service) => notification.with_service(service),
            None => notification,
        };

        Ok(self.notifications.publish(id, notification))
    }

    /// Starts replaying the configured notification values for one subscription.
    ///
    /// A no-op unless `auto_send_notifications` is on and the characteristic has
    /// values configured, so the default mock stays as quiet as the test expects.
    /// Re-subscribing replaces the loop instead of adding a second one, which would
    /// double the rate the test is asserting on.
    fn start_auto_send(
        &self,
        id: &DeviceId,
        characteristic: &CharacteristicUuid,
        service: Option<ServiceUuid>,
    ) {
        if !self.config.auto_send_notifications {
            return;
        }

        let Some(values) = self.config.notification_values.get(characteristic) else {
            return;
        };
        if values.is_empty() {
            return;
        }
        let values = values.clone();

        let key = (id.clone(), *characteristic);
        if let Some((_, previous)) = self.notification_tasks.remove(&key) {
            previous.abort();
        }

        let hub = self.notifications.clone();
        let tasks = self.notification_tasks.clone();
        let devices = self.devices.clone();
        let device_id = id.clone();
        let uuid = *characteristic;
        let interval = Duration::from_millis(self.config.notification_interval_ms.max(1));

        let task_key = key.clone();
        let handle = tokio::spawn(async move {
            let mut index = 0usize;
            loop {
                time::sleep(interval).await;

                let still_subscribed = devices
                    .get(&device_id)
                    .map(|entry| entry.subscribed.contains(&uuid))
                    .unwrap_or(false);
                if !still_subscribed {
                    tasks.remove(&task_key);
                    break;
                }

                hub.publish(
                    &device_id,
                    ValueNotification {
                        service,
                        ..ValueNotification::new(uuid, values[index % values.len()].clone())
                    },
                );
                index += 1;
            }
        });

        // Aborting whoever we displaced rather than leaving them running: two
        // concurrent subscribes to the same characteristic used to double the rate,
        // with only one of the two loops still reachable by `unsubscribe`.
        if let Some(previous) = self.notification_tasks.insert(key, handle) {
            previous.abort();
        }
    }

    /// Ends the auto-send loop for one subscription, if one is running.
    fn stop_auto_send(&self, id: &DeviceId, characteristic: &CharacteristicUuid) {
        if let Some((_, task)) = self
            .notification_tasks
            .remove(&(id.clone(), *characteristic))
        {
            task.abort();
        }
    }

    /// Open device event streams on this monitor.
    pub fn event_subscribers(&self) -> usize {
        self.events.subscriber_count()
    }

    /// Device events dropped so far because a consumer's channel was full.
    ///
    /// A lossy channel is the design (a radio pump must not block behind a slow
    /// consumer); this is what makes the loss something a test can assert and a node
    /// can alert on instead of a rumour.
    pub fn dropped_events(&self) -> u64 {
        self.events.dropped_events()
    }

    /// Open notification streams for one device.
    pub fn notification_subscribers(&self, id: &DeviceId) -> u64 {
        self.notifications.subscriber_count(id)
    }

    /// Notification frames dropped for one device because a consumer fell behind.
    pub fn dropped_notifications(&self, id: &DeviceId) -> u64 {
        self.notifications.dropped_count(id)
    }

    /// Put a device on the simulated air, once, and return what the listeners saw.
    ///
    /// This is the one place the mock decides what a report means: a device nobody
    /// has heard of is a discovery, a device whose tracked properties moved is an
    /// update naming them, and a device that merely turned up again is an
    /// [`DeviceEvent::Advertisement`] sighting. The self-advertising loop and any
    /// test driving a particular report both come through here, so the mock cannot
    /// mean one thing in the loop and another in the test.
    ///
    /// GATT state attached to the address (services, subscriptions) survives; only
    /// the advertisement snapshot is replaced.
    ///
    /// # Example
    ///
    /// ```
    /// use bt_mon::backends::mock::MockMonitor;
    /// use bt_mon::{BluetoothDevice, DeviceEvent, DeviceId, UpdateField};
    ///
    /// let monitor = MockMonitor::new();
    /// let device = BluetoothDevice::new(DeviceId::new("AA:BB:CC:DD:EE:FF"),
    ///     "AA:BB:CC:DD:EE:FF".to_string()).with_rssi(-70);
    ///
    /// // First heard: a discovery.
    /// assert!(matches!(monitor.advertise(device.clone()),
    ///     DeviceEvent::DeviceAdded { .. }));
    /// // Heard again with the same contents: still a sighting, not silence.
    /// assert!(matches!(monitor.advertise(device.clone()),
    ///     DeviceEvent::Advertisement { .. }));
    /// // Heard again with a different signal: an update that says so.
    /// let moved = monitor.advertise(device.with_rssi(-52));
    /// assert!(matches!(moved, DeviceEvent::DeviceUpdated { ref changed_fields, .. }
    ///     if changed_fields == &vec![UpdateField::Rssi]));
    /// ```
    pub fn advertise(&self, device: BluetoothDevice) -> DeviceEvent {
        let event = announce(&self.devices, device);
        self.publish(event.clone());
        event
    }

    /// Puts the configured advertisers on the air until the scan stops.
    ///
    /// Each tick re-advertises every device, as a real radio does. The first tick
    /// is a discovery and every tick after it is a sighting of a device that is
    /// still there; the node's sampling policy is what decides whether a sighting
    /// is worth a row. The device is registered with the monitor too, so
    /// `devices()` and the event stream agree.
    ///
    /// The loop owns the slot for as long as its generation is the current one. A
    /// `stop_scan` → `start_scan` pair inside one `advertise_interval_ms` never lets
    /// the old loop observe `advertising` go false, and without the generation check
    /// both loops would keep running, announcing every beacon twice per tick.
    fn spawn_advertisers(&self) {
        let devices = self.devices.clone();
        let events = self.events.clone();
        let advertising = self.advertising.clone();
        let generations = self.scan_generation.clone();
        let advertisers = self.config.advertisers.clone();
        let generation = self.scan_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let interval = Duration::from_millis(self.advertise_interval_ms());

        advertising.store(true, Ordering::SeqCst);
        tokio::spawn(async move {
            while advertising.load(Ordering::SeqCst)
                && generations.load(Ordering::SeqCst) == generation
            {
                for advertiser in &advertisers {
                    let device = BluetoothDevice::new(
                        advertiser.id.clone(),
                        advertiser.id.as_str().to_string(),
                    )
                    .with_name(advertiser.name.clone())
                    .with_rssi(advertiser.rssi)
                    .with_raw_payload(advertiser.advertisement_bytes());

                    events.publish(&announce(&devices, device));
                }

                time::sleep(interval).await;
            }
        });
    }

    /// The cadence the simulated radio advertises on, in milliseconds.
    ///
    /// `advertise_interval_ms` of `0` means "not configured" and falls back to the
    /// default; see [`MockConfig::advertise_interval_ms`] for why zero is not a
    /// request for a firehose, and [`advertise_interval_ms`] (the free function) for
    /// what a `MonitorConfig` scan interval maps to.
    fn advertise_interval_ms(&self) -> u64 {
        if self.config.advertise_interval_ms == 0 {
            MockConfig::default().advertise_interval_ms
        } else {
            self.config.advertise_interval_ms
        }
    }
}

/// Record one advertisement of `device` and say what the listeners should see.
///
/// An advertisement says what a device is broadcasting, not what link it has, so the
/// link state a previous report or a local `connect` put on the cached snapshot
/// survives: `is_connected` and `services_resolved` are carried over. Overwriting them
/// with the fresh snapshot's defaults would make a beacon the node is connected to
/// report a disconnection on every advertisement — a change of state that never
/// happened, on the one field consumers are most careful about.
///
/// Everything else about the GATT table (services, subscriptions, values) is untouched
/// for the same reason: a scan is not a re-configuration.
fn announce(devices: &DashMap<DeviceId, SimulatedDevice>, device: BluetoothDevice) -> DeviceEvent {
    let id = device.id.clone();
    if let Some(mut entry) = devices.get_mut(&id) {
        let previous = entry.value().device.clone();
        let mut reported = device;
        reported.is_connected = previous.is_connected;
        reported.services_resolved = previous.services_resolved;
        entry.value_mut().device = reported.clone();
        return report_event(&previous, reported);
    }

    devices.insert(id, SimulatedDevice::new(device.clone()));
    DeviceEvent::DeviceAdded { device }
}

/// Stopping the advertiser loop and every notification loop on drop keeps a discarded
/// monitor from broadcasting forever: the tasks hold `Arc` clones, so dropping the
/// monitor is the only signal they have that nobody is listening any more. Bumping the
/// generation is what reaches a loop that never observes `advertising` go false.
impl Drop for MockMonitor {
    fn drop(&mut self) {
        self.advertising.store(false, Ordering::SeqCst);
        self.scan_generation.fetch_add(1, Ordering::SeqCst);

        // `retain` rather than iterating and removing: removing a key while holding an
        // `iter_mut()` guard deadlocks the shard. Aborting inside the closure is both
        // the side effect and the predicate's `false`.
        self.notification_tasks.retain(|_, task| {
            task.abort();
            false
        });

        self.notifications.forget_all();
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
            // The radio refusing to start is a backend failure, not a failure to
            // build the monitor: the monitor exists and works, this call did not.
            return Err(Error::BackendError {
                backend: BackendKind::Mock,
                message: self.config.scan_error_message.clone(),
            });
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
        // Retires the current advertiser loop even if it never observes
        // `advertising` go false before the next `start_scan` sets it true again.
        self.scan_generation.fetch_add(1, Ordering::SeqCst);
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
        // Replay the current devices so a stream opened late sees the existing state
        // (mirrors btleplug, whose platform stream synthesizes DeviceDiscovered for
        // devices the daemon already knows when it opens).
        let replay = self
            .devices
            .iter()
            .map(|entry| DeviceEvent::DeviceAdded {
                device: entry.value().device.clone(),
            })
            .collect::<Vec<_>>();

        // The stream stays open for the lifetime of the monitor; it completes when the
        // monitor is dropped or this consumer is, whichever comes first.
        Ok(self.events.subscribe_seeded(replay))
    }

    async fn is_scanning(&self) -> Result<bool> {
        let scanning = self.scanning.lock().await;
        Ok(*scanning)
    }

    /// What the mock can honestly say about itself, so that a consumer's health handling is
    /// exercised by the same tests that exercise the real backends.
    ///
    /// `discovering` follows the rule the real backends use — requested *and* able to
    /// deliver — which makes `MockConfig::adapter_powered(false)` the way to test the TD-13
    /// case: an operator asked for a scan and nothing is coming. The re-arm and stream-open
    /// counters stay zero because the mock has no re-arm to fail and no platform stream to
    /// open; unlike a backend that returns `None`, that is a statement about this backend
    /// rather than an absence of measurement.
    fn health(&self) -> Option<MonitorHealth> {
        let adapter_powered = self.config.adapter_powered;
        // The scanning flag is behind an async mutex; `health()` is sync because a consumer
        // in a log line cannot await. Contending with a `start_scan` in flight is the only
        // way this reads false when it should read true, and the answer one call later is
        // correct.
        let scanning_requested = self
            .scanning
            .try_lock()
            .map(|scanning| *scanning)
            .unwrap_or(false);

        Some(MonitorHealth {
            adapter_powered,
            scanning_requested,
            discovering: scanning_requested && adapter_powered,
            ..MonitorHealth::default()
        })
    }
}

#[async_trait]
impl GattClient for MockMonitor {
    async fn connect(&self, id: &DeviceId) -> Result<()> {
        debug!("Connecting to mock device: {}", id);

        if self.config.error_on_connect {
            return Err(Error::BackendError {
                backend: BackendKind::Mock,
                message: self.config.connect_error_message.clone(),
            });
        }

        // Existence is checked without keeping a guard: a `DashMap` read guard
        // held across `get_mut` below (or across the await) deadlocks on the
        // same shard, which is what this used to do — every test that connected
        // to a device hung forever.
        if !self.devices.contains_key(id) {
            return Err(Error::DeviceNotFound(id.clone()));
        }

        self.simulate_delay(self.config.connect_delay_ms).await;

        // A real radio reports the link it just made: BlueZ flips the `Connected`
        // property, btleplug delivers `CentralEvent::DeviceEvent { connected: true }`,
        // and in both cases the caller's `connect()` and the event pump are separate
        // paths to the same cache write. A mock that changed the cache in silence
        // taught consumers that only a *remote* change reaches the stream, which is
        // the opposite of what the stream is for.
        self.report_change(self.update_device(id, |device| device.is_connected = true));

        info!("Connected to mock device: {}", id);
        Ok(())
    }

    async fn disconnect(&self, id: &DeviceId) -> Result<()> {
        debug!("Disconnecting from mock device: {}", id);

        let change = self.update_device(id, |device| device.is_connected = false);
        // The link carried the subscription: with it gone the CCCD is gone too, so
        // the auto-send loops stop and open notification streams end. A consumer
        // that reconnects subscribes again, exactly as it would on a real
        // peripheral.
        self.forget_device(id);
        self.report_change(change);

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

        let services = {
            let entry = self
                .devices
                .get(id)
                .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

            if !entry.value().device.is_connected {
                return Err(Error::NotConnected(id.clone()));
            }

            public_services(&entry.value().services)
        };

        // Discovery is what makes the table real. `services_resolved` is the consumer's
        // signal that there is a GATT table behind `services()`, so a mock that never
        // set it could not be used to test anything that waits for resolution — and
        // setting it here reports the transition like a real adapter would.
        self.report_change(self.update_device(id, |device| device.services_resolved = true));

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
        Ok(public_services(&entry.value().services))
    }

    async fn read_characteristic(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
    ) -> Result<Vec<u8>> {
        debug!("Reading characteristic {} from mock device {}", uuid, id);

        if self.config.error_on_read {
            return Err(Error::GattError {
                device_id: id.clone(),
                operation: "read_characteristic".to_string(),
                message: self.config.read_error_message.clone(),
            });
        }

        self.simulate_delay(self.config.read_delay_ms).await;

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

        if !entry.value().device.is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        let characteristic = find_characteristic(&entry.value().services, uuid)
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        // Both gates apply: `properties` is what the peripheral advertises it can do,
        // `allow_read` is the harness's veto. A read of a characteristic that declares
        // no read property fails the way a real ATT server answers with
        // "Read Not Permitted" — it does not hand back an empty vector, which is what
        // this used to do for *every* read, readable or not.
        if !characteristic.allow_read || !characteristic.properties.read {
            return Err(Error::InvalidArgument(format!(
                "Characteristic {uuid} on device {id} is not readable"
            )));
        }

        let value = entry.value().values.get(uuid).cloned().unwrap_or_default();
        drop(entry);

        debug!(
            "Read {} bytes from mock characteristic {}",
            value.len(),
            uuid
        );
        Ok(value)
    }

    async fn write_characteristic(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
        value: &[u8],
        response: bool,
    ) -> Result<()> {
        debug!(
            "Writing to characteristic {} on mock device {} ({} bytes)",
            uuid,
            id,
            value.len()
        );

        if self.config.error_on_write {
            return Err(Error::GattError {
                device_id: id.clone(),
                operation: "write_characteristic".to_string(),
                message: self.config.write_error_message.clone(),
            });
        }

        self.simulate_delay(self.config.write_delay_ms).await;

        let mut entry = self
            .devices
            .get_mut(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

        if !entry.value().device.is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        // Which write property the call needs depends on whether the caller asked for
        // a response: a characteristic that is write-without-response only rejects a
        // write *request*. A mock that ignored `response` would let a caller ship code
        // that only works because the mock never enforced the difference.
        let (permitted, kind) = {
            let characteristic = find_characteristic(&entry.value().services, uuid)
                .ok_or(Error::CharacteristicNotFound(*uuid))?;
            let kind = if response {
                "write"
            } else {
                "write-without-response"
            };
            let declared = if response {
                characteristic.properties.write
            } else {
                characteristic.properties.write_without_response
            };
            (characteristic.allow_write && declared, kind)
        };

        if !permitted {
            return Err(Error::InvalidArgument(format!(
                "Characteristic {uuid} on device {id} does not support {kind}"
            )));
        }

        // The write lands in the value store, so the next read of that characteristic
        // returns what was written — the round-trip a real peripheral gives and this
        // one could not, because nothing stored the bytes.
        entry.value_mut().values.insert(*uuid, value.to_vec());
        drop(entry);

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

        let service = {
            let entry = self
                .devices
                .get(id)
                .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;

            if !entry.value().device.is_connected {
                return Err(Error::NotConnected(id.clone()));
            }

            let characteristic = find_characteristic(&entry.value().services, uuid)
                .ok_or(Error::CharacteristicNotFound(*uuid))?;

            if !characteristic.allow_subscribe || !characteristic.notifies() {
                return Err(Error::InvalidArgument(format!(
                    "Characteristic {uuid} on device {id} does not notify"
                )));
            }

            service_of(&entry.value().services, uuid)
        };

        if let Some(mut entry) = self.devices.get_mut(id) {
            entry.value_mut().subscribed.insert(*uuid);
        }

        self.start_auto_send(id, uuid, service);

        debug!("Successfully subscribed to characteristic {}", uuid);
        Ok(())
    }

    async fn unsubscribe(&self, id: &DeviceId, uuid: &CharacteristicUuid) -> Result<()> {
        debug!(
            "Unsubscribing from characteristic {} on mock device {}",
            uuid, id
        );

        if let Some(mut entry) = self.devices.get_mut(id) {
            entry.value_mut().subscribed.remove(uuid);
        }

        // Stopping the loop is what makes this mean something. Before, `unsubscribe`
        // cleared a flag nobody read and the simulated peripheral went on notifying a
        // caller that had asked it to stop.
        self.stop_auto_send(id, uuid);

        debug!("Successfully unsubscribed from characteristic {}", uuid);
        Ok(())
    }

    async fn notifications(&self, id: &DeviceId) -> Result<NotificationStream> {
        debug!("Setting up notification stream for mock device {}", id);

        // An id this monitor has never heard of is a mistake, not a device that will
        // notify later: open the shared hub's stream for a device it does know.
        if !self.devices.contains_key(id) {
            return Err(Error::DeviceNotFound(id.clone()));
        }

        Ok(self.notifications.subscribe(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::events::UpdateField;
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
        assert!(
            matches!(
                result,
                Err(Error::BackendError {
                    backend: BackendKind::Mock,
                    ..
                })
            ),
            "a radio that refused to scan is a backend failure, not a monitor that \
             could never exist: {result:?}"
        );
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
        match result {
            Err(Error::BackendError {
                backend: BackendKind::Mock,
                message,
            }) => assert_eq!(message, monitor.config.connect_error_message),
            other => panic!(
                "a simulated link failure names the backend and carries the configured \
                 message: {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn a_configured_read_value_is_what_read_returns() {
        let monitor = MockMonitor::with_config(
            MockConfig::default()
                .with_connect_delay_ms(1)
                .with_read_delay_ms(1),
        );
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new().with_read(true),
        )
        .with_read_value(b"battery 87%".to_vec());
        let (device_id, _service, uuid) = connected_peripheral(&monitor, characteristic).await;

        assert_eq!(
            monitor
                .read_characteristic(&device_id, &uuid)
                .await
                .unwrap(),
            b"battery 87%",
            "the mock used to hand back an empty Vec for every read, whatever was \
             configured"
        );
    }

    #[tokio::test]
    async fn a_read_the_characteristic_does_not_support_fails() {
        // Unknown uuid, unreadable properties, and the harness's own veto are three
        // different mistakes; none of them is an empty read.
        let monitor = MockMonitor::new();
        let readable = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new().with_read(true),
        );
        let (device_id, _service, _uuid) = connected_peripheral(&monitor, readable).await;

        let unknown =
            CharacteristicUuid::parse_str("0000ffff-0000-1000-8000-00805f9b34fb").unwrap();
        assert!(
            matches!(
                monitor.read_characteristic(&device_id, &unknown).await,
                Err(Error::CharacteristicNotFound(_))
            ),
            "a uuid the table does not have is not a characteristic that reads as nothing"
        );

        let monitor = MockMonitor::new();
        let write_only = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new().with_write(true),
        );
        let (device_id, _service, uuid) = connected_peripheral(&monitor, write_only).await;
        assert!(
            matches!(
                monitor.read_characteristic(&device_id, &uuid).await,
                Err(Error::InvalidArgument(_))
            ),
            "a characteristic with no read property must not read"
        );

        let monitor = MockMonitor::new();
        let vetoed = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new().with_read(true),
        )
        .with_allow_read(false);
        let (device_id, _service, uuid) = connected_peripheral(&monitor, vetoed).await;
        assert!(
            matches!(
                monitor.read_characteristic(&device_id, &uuid).await,
                Err(Error::InvalidArgument(_))
            ),
            "allow_read(false) has to reach the behaviour"
        );
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
        match result {
            Err(Error::GattError {
                device_id: reported,
                operation,
                message,
            }) => {
                assert_eq!(&reported, &device_id);
                assert_eq!(operation, "read_characteristic");
                assert_eq!(message, monitor.config.read_error_message);
            }
            other => panic!("a simulated read failure should name the call, got {other:?}"),
        }
    }

    /// A connected device carrying exactly one characteristic.
    ///
    /// Most GATT assertions need a device whose table says what the characteristic
    /// can do, and building that by hand in every test buries the assertion.
    async fn connected_peripheral(
        monitor: &MockMonitor,
        characteristic: SimulatedCharacteristic,
    ) -> (DeviceId, ServiceUuid, CharacteristicUuid) {
        let device_id = DeviceId::new("00:11:22:33:44:55");
        let service = ServiceUuid::parse_str("0000180f-0000-1000-8000-00805f9b34fb").unwrap();
        let uuid = characteristic.uuid;

        monitor
            .add_device_with_services(
                device_id.clone(),
                "Sensor",
                vec![SimulatedService::new(service, true).with_characteristic(characteristic)],
            )
            .await
            .unwrap();
        monitor.connect(&device_id).await.unwrap();

        (device_id, service, uuid)
    }

    fn characteristic_uuid(short: &str) -> CharacteristicUuid {
        CharacteristicUuid::parse_str(&format!("0000{short}-0000-1000-8000-00805f9b34fb")).unwrap()
    }

    #[tokio::test]
    async fn a_write_is_what_the_next_read_returns() {
        let monitor = MockMonitor::with_config(
            MockConfig::default()
                .with_connect_delay_ms(1)
                .with_write_delay_ms(1),
        );
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new()
                .with_read(true)
                .with_write(true)
                .with_write_without_response(true),
        )
        .with_read_value(b"before".to_vec());
        let (device_id, _service, uuid) = connected_peripheral(&monitor, characteristic).await;

        monitor
            .write_characteristic(&device_id, &uuid, b"after", true)
            .await
            .unwrap();

        assert_eq!(
            monitor
                .read_characteristic(&device_id, &uuid)
                .await
                .unwrap(),
            b"after",
            "a write that goes nowhere is not a GATT write"
        );
    }

    #[tokio::test]
    async fn a_write_the_characteristic_does_not_support_fails() {
        // `response` is not decoration: a characteristic that only accepts a
        // write request must refuse a command, or code written against the mock
        // ships with a bug the radio will report.
        let monitor = MockMonitor::new();
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new().with_write(true),
        );
        let (device_id, _service, uuid) = connected_peripheral(&monitor, characteristic).await;

        let response = monitor
            .write_characteristic(&device_id, &uuid, b"ok", true)
            .await;
        assert!(response.is_ok(), "write-with-response is declared");

        let command = monitor
            .write_characteristic(&device_id, &uuid, b"no", false)
            .await;
        assert!(
            matches!(command, Err(Error::InvalidArgument(_))),
            "write-without-response is not: {command:?}"
        );

        let monitor = MockMonitor::new();
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new().with_write_without_response(true),
        )
        .with_allow_write(false);
        let (device_id, _service, uuid) = connected_peripheral(&monitor, characteristic).await;

        assert!(
            matches!(
                monitor
                    .write_characteristic(&device_id, &uuid, b"no", false)
                    .await,
                Err(Error::InvalidArgument(_))
            ),
            "allow_write(false) has to reach the behaviour"
        );
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
        match result {
            Err(Error::GattError { operation, .. }) => {
                assert_eq!(operation, "write_characteristic");
            }
            other => panic!("a simulated write failure should name the call, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_subscription_needs_a_characteristic_that_notifies() {
        let monitor = MockMonitor::with_config(MockConfig::default().with_connect_delay_ms(1));
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a37"),
            CharacteristicProperties::new().with_notify(true),
        );
        let (device_id, _service, uuid) = connected_peripheral(&monitor, characteristic).await;

        monitor.subscribe(&device_id, &uuid).await.unwrap();
        monitor.unsubscribe(&device_id, &uuid).await.unwrap();

        // The same call on a characteristic that cannot deliver anything is a
        // configuration mistake, and it used to succeed silently — leaving a test
        // to wait forever for a frame that could never arrive.
        let monitor = MockMonitor::new();
        let read_only = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new().with_read(true),
        );
        let (device_id, _service, uuid) = connected_peripheral(&monitor, read_only).await;

        assert!(
            matches!(
                monitor.subscribe(&device_id, &uuid).await,
                Err(Error::InvalidArgument(_))
            ),
            "a characteristic with no notify property cannot be subscribed"
        );

        let monitor = MockMonitor::new();
        let forbidden = SimulatedCharacteristic::new(
            characteristic_uuid("2a37"),
            CharacteristicProperties::new().with_notify(true),
        )
        .with_allow_subscribe(false);
        let (device_id, _service, uuid) = connected_peripheral(&monitor, forbidden).await;

        assert!(
            matches!(
                monitor.subscribe(&device_id, &uuid).await,
                Err(Error::InvalidArgument(_))
            ),
            "allow_subscribe(false) has to reach the behaviour"
        );
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

    #[tokio::test]
    async fn a_scan_on_an_unpowered_adapter_is_not_discovering() {
        // The TD-13 case, exercisable without a radio: the operator asked for a scan and
        // the health report has to say that nothing is coming rather than agree that a scan
        // is running.
        let monitor = MockMonitor::with_config(MockConfig::default().with_adapter_powered(false));
        let health = monitor
            .health()
            .expect("the mock reports its own health rather than leaving it unmeasured");
        assert!(!health.adapter_powered);
        assert!(!health.scanning_requested);

        monitor.start_scan().await.unwrap();
        let health = monitor.health().unwrap();
        assert!(
            health.scanning_requested,
            "the request is recorded even though it cannot be honoured"
        );
        assert!(
            !health.discovering,
            "a scan started on an adapter that is off is not a scan that is happening"
        );
    }

    #[tokio::test]
    async fn a_scan_on_a_powered_adapter_is_discovering() {
        let monitor = MockMonitor::new();
        assert!(
            !monitor.health().unwrap().discovering,
            "nobody has asked yet"
        );

        monitor.start_scan().await.unwrap();
        let health = monitor.health().unwrap();
        assert!(health.scanning_requested);
        assert!(health.discovering);

        monitor.stop_scan().await.unwrap();
        assert!(!monitor.health().unwrap().discovering);
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

    fn beacon() -> BluetoothDevice {
        BluetoothDevice::new(
            DeviceId::new("F0:EE:00:00:01:02"),
            "F0:EE:00:00:01:02".to_string(),
        )
        .with_name("Meter Beacon")
        .with_rssi(-67)
        .with_manufacturer_data(0x004C, vec![0x01])
    }

    #[test]
    fn a_beacon_heard_twice_with_nothing_said_is_still_heard() {
        // The B14 case at the radio's own level: a beacon that has not moved,
        // whose signal strength and payload are identical, used to disappear
        // from the event stream after its first advertisement.
        let monitor = MockMonitor::new();
        assert!(
            matches!(monitor.advertise(beacon()), DeviceEvent::DeviceAdded { .. }),
            "the first report of a device is a discovery"
        );

        match monitor.advertise(beacon()) {
            DeviceEvent::Advertisement { device } => {
                assert_eq!(device.id.as_str(), "F0:EE:00:00:01:02");
            }
            other => panic!("a repeat report should be a sighting, got {other:?}"),
        }
    }

    #[test]
    fn a_beacon_that_changes_its_advertisement_changes_the_report() {
        // A sensor that puts a new reading into its manufacturer data is news.
        // It used to update the cache and emit nothing, so a device saying
        // something new produced exactly the same silence as one saying nothing.
        let monitor = MockMonitor::new();
        monitor.advertise(beacon());

        let reading = beacon().with_manufacturer_data(0x004C, vec![0x02]);
        match monitor.advertise(reading) {
            DeviceEvent::DeviceUpdated {
                device,
                changed_fields,
            } => {
                assert_eq!(changed_fields, vec![UpdateField::ManufacturerData]);
                assert_eq!(
                    device.manufacturer_data.get(&0x004C),
                    Some(&vec![0x02]),
                    "the report carries the new reading, not the cached one"
                );
            }
            other => panic!("a changed advertisement should be an update, got {other:?}"),
        }
    }

    #[test]
    fn advertising_keeps_the_gatt_state_attached_to_an_address() {
        // Replacing the advertisement snapshot must not wipe the services
        // something else attached to the same device.
        let monitor = MockMonitor::new();
        monitor.advertise(beacon());

        let id = DeviceId::new("F0:EE:00:00:01:02");
        let uuid = ServiceUuid::parse_str("0000180f-0000-1000-8000-00805f9b34fb").unwrap();
        monitor
            .devices
            .get_mut(&id)
            .expect("the advertisement registered the device")
            .value_mut()
            .services
            .push(SimulatedService::new(uuid, true));

        monitor.advertise(beacon().with_rssi(-55));

        let entry = monitor.devices.get(&id).expect("device still known");
        assert_eq!(
            entry.value().services.len(),
            1,
            "the service survived the re-advertisement"
        );
        assert_eq!(
            entry.value().device.rssi,
            Some(-55),
            "and the advertisement snapshot moved"
        );
    }

    #[tokio::test]
    async fn a_stationary_beacon_keeps_being_heard_by_an_open_stream() {
        // What B14 looks like from the consumer's side: one beacon, nothing
        // changing about it, and a stream that keeps saying it is there.
        let monitor = MockMonitor::with_config(
            MockConfig::default()
                .with_scan_delay_ms(0)
                .with_advertisers(vec![SimulatedAdvertiser::new(
                    "F0:EE:00:00:03:03",
                    "Still Here",
                )])
                .with_advertise_interval_ms(20),
        );
        let mut events = monitor.device_events().await.unwrap();
        monitor.start_scan().await.unwrap();

        let mut kinds = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.next()).await {
            kinds.push(match event {
                DeviceEvent::DeviceAdded { .. } => "added",
                DeviceEvent::Advertisement { .. } => "advertisement",
                DeviceEvent::DeviceUpdated { .. } => "updated",
                DeviceEvent::DeviceRemoved { .. } => "removed",
            });
        }

        assert!(
            kinds.len() >= 3,
            "a beacon advertising every 20 ms should be heard repeatedly, heard {kinds:?}"
        );
        assert_eq!(
            kinds.first().copied(),
            Some("added"),
            "the first report of a device is a discovery"
        );
        assert!(
            kinds[1..].iter().all(|kind| *kind == "advertisement"),
            "nothing about the beacon moves, so every report after the first is \
             a sighting rather than an update or a repeat discovery: {kinds:?}"
        );
    }

    #[tokio::test]
    async fn a_link_the_caller_made_is_reported_like_any_other_change() {
        // A consumer that drives off the event stream rather than polling has to hear
        // about the link its own code asked for. Both real backends report it — BlueZ
        // flips the `Connected` property, btleplug posts a device event — so a mock
        // that stayed silent was teaching consumers a rule the radio does not follow.
        let monitor = MockMonitor::with_config(MockConfig::default().with_connect_delay_ms(1));
        let device_id = DeviceId::new("00:11:22:33:44:55");
        monitor.add_device(device_id.clone()).await.unwrap();

        let mut events = monitor.device_events().await.unwrap();
        let replayed = next_event(&mut events, Duration::from_secs(1)).await;
        assert!(
            matches!(replayed, Some(DeviceEvent::DeviceAdded { .. })),
            "the device that was already there is replayed: {replayed:?}"
        );

        monitor.connect(&device_id).await.unwrap();
        monitor.connect(&device_id).await.unwrap();
        monitor.disconnect(&device_id).await.unwrap();

        let mut links = Vec::new();
        while let Some(event) = next_event(&mut events, Duration::from_millis(200)).await {
            match event {
                DeviceEvent::DeviceUpdated {
                    device,
                    changed_fields,
                } => {
                    assert_eq!(
                        changed_fields,
                        vec![UpdateField::Connected],
                        "the only thing that moved is the link"
                    );
                    links.push(device.is_connected);
                }
                other => panic!("a link change is an update, got {other:?}"),
            }
        }

        assert_eq!(
            links,
            vec![true, false],
            "connecting twice is one change, and the disconnect is the second"
        );
    }

    #[tokio::test]
    async fn discovery_reports_that_the_service_table_resolved() {
        let monitor = MockMonitor::new();
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new().with_read(true),
        );
        let (device_id, _service, _uuid) = connected_peripheral(&monitor, characteristic).await;

        let mut events = monitor.device_events().await.unwrap();
        let replayed = next_event(&mut events, Duration::from_secs(1))
            .await
            .expect("the device is replayed");
        match replayed {
            DeviceEvent::DeviceAdded { device } => assert!(
                !device.services_resolved,
                "nothing has resolved before discovery runs"
            ),
            other => panic!("expected the replayed discovery, got {other:?}"),
        }

        let table = monitor.discover_services(&device_id).await.unwrap();
        assert_eq!(table.len(), 1);

        let event = next_event(&mut events, Duration::from_secs(1))
            .await
            .expect("resolution is a change worth reporting");
        match event {
            DeviceEvent::DeviceUpdated {
                device,
                changed_fields,
            } => {
                assert_eq!(changed_fields, vec![UpdateField::ServicesResolved]);
                assert!(device.services_resolved);
            }
            other => panic!("discovery resolving the table is an update, got {other:?}"),
        }

        assert!(
            monitor.device(&device_id).await.unwrap().services_resolved,
            "and the cache agrees with the report"
        );
    }

    #[tokio::test]
    async fn every_caller_of_notifications_gets_the_frame() {
        let monitor = MockMonitor::new();
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a37"),
            CharacteristicProperties::new().with_notify(true),
        );
        let (device_id, service, uuid) = connected_peripheral(&monitor, characteristic).await;

        let mut first = monitor.notifications(&device_id).await.unwrap();
        let mut second = monitor.notifications(&device_id).await.unwrap();
        assert_eq!(monitor.notification_subscribers(&device_id), 2);

        assert_eq!(
            monitor
                .send_notification(&device_id, &uuid, vec![0, 62])
                .unwrap(),
            2,
            "both streams are live"
        );

        for stream in [&mut first, &mut second] {
            let frame = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("delivered")
                .expect("stream open");
            assert_eq!(frame.as_slice(), &[0, 62]);
            assert_eq!(frame.characteristic, uuid);
            assert_eq!(
                frame.service,
                Some(service),
                "a multiplexed device has to say which service the frame came from"
            );
        }

        // The defect this replaces keyed the senders by device: the second
        // `notifications()` call handed back a stream and silently closed the first
        // caller's.
        drop(first);
        assert_eq!(
            monitor
                .send_notification(&device_id, &uuid, vec![7])
                .unwrap(),
            1,
            "the stream that is left still receives"
        );
        let frame = tokio::time::timeout(Duration::from_secs(1), second.next())
            .await
            .expect("still open after the other caller left")
            .expect("stream open");
        assert_eq!(frame.as_slice(), &[7]);
    }

    #[tokio::test]
    async fn auto_send_replays_the_configured_values_and_stops_when_asked() {
        let uuid = characteristic_uuid("2a37");
        let monitor = MockMonitor::with_config(
            MockConfig::default()
                .with_connect_delay_ms(1)
                .with_auto_send_notifications(true)
                .with_notification_interval_ms(20)
                .with_notification_value(uuid, vec![1])
                .with_notification_value(uuid, vec![2]),
        );
        let characteristic =
            SimulatedCharacteristic::new(uuid, CharacteristicProperties::new().with_notify(true));
        let (device_id, service, uuid) = connected_peripheral(&monitor, characteristic).await;

        let mut frames = monitor.notifications(&device_id).await.unwrap();
        monitor.subscribe(&device_id, &uuid).await.unwrap();

        for expected in [1u8, 2, 1] {
            let frame = tokio::time::timeout(Duration::from_secs(2), frames.next())
                .await
                .unwrap_or_else(|_| panic!("the configured loop should deliver {expected}"))
                .expect("stream open");
            assert_eq!(frame.as_slice(), &[expected]);
            assert_eq!(frame.service, Some(service));
        }

        monitor.unsubscribe(&device_id, &uuid).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), frames.next())
                .await
                .is_err(),
            "unsubscribe has to stop the simulated peripheral talking"
        );
    }

    #[tokio::test]
    async fn a_disconnect_ends_the_notification_streams_it_carried() {
        let monitor = MockMonitor::new();
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a37"),
            CharacteristicProperties::new().with_notify(true),
        );
        let (device_id, _service, uuid) = connected_peripheral(&monitor, characteristic).await;

        let mut frames = monitor.notifications(&device_id).await.unwrap();
        monitor.subscribe(&device_id, &uuid).await.unwrap();
        assert_eq!(
            monitor
                .send_notification(&device_id, &uuid, vec![1])
                .unwrap(),
            1
        );
        frames.next().await.expect("the frame before the link went");

        monitor.disconnect(&device_id).await.unwrap();

        assert!(
            tokio::time::timeout(Duration::from_secs(1), frames.next())
                .await
                .expect("the stream ends rather than hanging")
                .is_none(),
            "the link carried the subscription, so its end closes the stream"
        );
    }

    #[tokio::test]
    async fn a_scan_restarted_inside_one_interval_does_not_double_the_radio() {
        let monitor = MockMonitor::with_config(
            MockConfig::default()
                .with_scan_delay_ms(0)
                .with_advertisers(vec![SimulatedAdvertiser::new(
                    "F0:EE:00:00:07:07",
                    "One Beacon",
                )])
                .with_advertise_interval_ms(50),
        );
        let mut events = monitor.device_events().await.unwrap();

        monitor.start_scan().await.unwrap();
        monitor.stop_scan().await.unwrap();
        monitor.start_scan().await.unwrap();

        let mut heard = 0usize;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(320);
        while let Ok(Some(_)) = tokio::time::timeout_at(deadline, events.next()).await {
            heard += 1;
        }

        // 320 ms of one beacon advertising every 50 ms is about six reports. The
        // defect left the first loop running beside the second: not a slightly wrong
        // count, a permanently doubled one, which is what a duty-cycle or rate
        // assertion downstream would have to live with.
        assert!(
            (3..=8).contains(&heard),
            "one advertiser at 50 ms should be heard roughly six times in 320 ms, \
             heard {heard} — two loops would be about twelve"
        );
    }

    #[test]
    fn the_monitor_config_decides_the_mocks_cadence_and_adapter() {
        let configured = MonitorConfig::new()
            .with_adapter("hci1")
            .with_scan_interval(Duration::from_millis(250));
        let no_interval = MonitorConfig::new().with_scan_interval(Duration::ZERO);
        let absurd = MonitorConfig::new().with_scan_interval(Duration::MAX);

        let mock = MockConfig::from(&configured);
        assert_eq!(mock.adapter_name, "hci1");
        assert_eq!(
            mock.advertise_interval_ms, 250,
            "`scan_interval` is how often the radio is heard, whatever backend hears it"
        );

        // Zero means "the operator never set an interval", not "shout as fast as the
        // task loop allows": a mock that advertised continuously would flood every
        // test that inherited the default config.
        // Zero is carried across as "unset", not "advertise as fast as the loop
        // allows" — which is what a bare `sleep(Duration::ZERO)` would become. The
        // monitor resolves it to its own default cadence, below.
        assert_eq!(MockConfig::from(&no_interval).advertise_interval_ms, 0);

        // A duration too large to count in milliseconds is one very long report, not
        // a wrap-around to zero and a firehose.
        assert_eq!(
            MockConfig::from(&absurd).advertise_interval_ms,
            u64::MAX,
            "saturating is fine; wrapping would be a firehose"
        );

        let monitor = MockMonitor::with_monitor_config(&no_interval);
        assert_eq!(
            monitor.advertise_interval_ms(),
            MockConfig::default().advertise_interval_ms,
            "the monitor applies the same rule it was handed"
        );
    }

    #[tokio::test]
    async fn the_service_table_a_caller_sees_is_the_one_configured() {
        let monitor = MockMonitor::new();
        let characteristic = SimulatedCharacteristic::new(
            characteristic_uuid("2a19"),
            CharacteristicProperties::new()
                .with_read(true)
                .with_notify(true),
        )
        .with_handle(0x0012);
        let (device_id, service, uuid) = connected_peripheral(&monitor, characteristic).await;

        let table = monitor.discover_services(&device_id).await.unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(table[0].uuid, service);
        assert!(table[0].is_primary);

        let found = table[0]
            .characteristics
            .iter()
            .find(|characteristic| characteristic.uuid == uuid)
            .expect("the configured characteristic is in the table");
        assert_eq!(
            found.handle,
            Some(0x0012),
            "the simulated handle survives the conversion"
        );
        assert!(found.can_read() && found.can_notify());
    }
}
