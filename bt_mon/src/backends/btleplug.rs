//! btleplug backend implementation for bt_mon.
//!
//! This module provides the cross-platform implementation using the `btleplug` crate.
//!
//! # What one monitor owns
//!
//! One platform central event stream, whatever number of consumers call
//! [`device_events`](crate::DeviceMonitor::device_events): the pump feeds a shared
//! [`DeviceEventSinks`] registry, so opening a
//! second stream does not open a second D-Bus subscription, and a stream that is dropped
//! releases its slot immediately rather than when the radio next says something. Dropped
//! events are counted ([`BtleplugMonitor::dropped_events`]) because the channel is
//! lossy by design.
//!
//! One notification reader per device likewise serves every
//! [`notifications`](crate::GattClient::notifications) call for that device, starting when
//! the first consumer arrives and stopping when the last one leaves.
//!
//! # Reading this file without a radio
//!
//! The decisions — how a platform report becomes an event, what a set of
//! `PeripheralProperties` means, how a platform error maps onto
//! [`Error`] — are pure functions over plain data (`properties_to_device`,
//! `map_gatt_error`, `update_connected`), and the platform call sits behind them. That is
//! deliberate: with no adapter in this container, the code that used to be untestable is
//! the code the tests now drive.

use async_trait::async_trait;
use btleplug::api::{
    Central as _, CentralEvent, CentralState, Manager as _, Peripheral as _, ScanFilter,
    ValueNotification as BtleplugNotification, WriteType,
};
use dashmap::DashMap;
use futures::stream::StreamExt;
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time;

use crate::config::{adapter_matches, MonitorConfig};
use crate::error::{BackendKind, Error, Result};
use crate::monitor::events::{
    report_event, DeviceEvent, DeviceEventSinks, DeviceEventStream, NotificationHub,
    NotificationStream,
};
use crate::monitor::health::{HealthState, MonitorHealth};
use crate::monitor::{DeviceMonitor, GattClient};
use crate::types::{
    BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId, GattCharacteristic,
    GattService, ServiceUuid, ValueNotification,
};

/// How long to wait before reopening a platform event stream that ended.
///
/// Short enough that a `bluetoothd` restart costs a beat of reports, long enough that a
/// stack which refuses the subscription outright does not turn the pump into a spin loop.
const EVENT_STREAM_RETRY: Duration = Duration::from_secs(2);

/// Internal representation of a discovered device.
#[derive(Clone, Debug)]
struct DiscoveredDevice {
    device: BluetoothDevice,
    peripheral: btleplug::platform::Peripheral,
}

/// btleplug backend implementation of DeviceMonitor and GattClient.
pub struct BtleplugMonitor {
    adapter: Arc<Mutex<btleplug::platform::Adapter>>,
    devices: Arc<DashMap<DeviceId, DiscoveredDevice>>,
    /// Someone has asked for a scan. Distinct from whether the radio is delivering one:
    /// see [`DeviceMonitor::is_scanning`] and [`Self::health`].
    scanning: Arc<AtomicBool>,
    /// Bumped by every `start_scan`, so the refresher task belonging to an
    /// earlier scan exits instead of leaving one running per restart.
    scan_generation: Arc<AtomicU64>,
    /// How often the running scan is re-armed; see [`MonitorConfig::scan_interval`].
    scan_interval: Duration,
    /// Fan-out to every `device_events()` consumer, plus the count of what they could
    /// not keep up with.
    events: DeviceEventSinks,
    /// The single pump that feeds `events`, started on the first subscription so a
    /// monitor nobody reads events from does not hold a platform stream open.
    event_pump: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Fan-out for GATT notifications, one channel per device.
    notifications: NotificationHub,
    /// The platform notification reader per device, kept so a second
    /// `notifications()` call reuses it instead of opening a second one.
    notification_pumps: Arc<DashMap<DeviceId, JoinHandle<()>>>,
    health: Arc<HealthState>,
    /// Set on drop, which is the only stop signal the tasks have: they hold `Arc`
    /// clones of everything else they need.
    shutting_down: Arc<AtomicBool>,
}

impl BtleplugMonitor {
    /// Create a new btleplug monitor with the default configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the btleplug manager cannot be initialized
    /// or if no Bluetooth adapter is found.
    pub async fn new() -> Result<Self> {
        Self::with_config(MonitorConfig::default()).await
    }

    /// Create a btleplug monitor on the adapter and scan cadence `config` asks for.
    ///
    /// # Errors
    ///
    /// Returns an error if the btleplug manager cannot be initialized, if no
    /// Bluetooth adapter is found, or — when [`MonitorConfig::adapter`] is set —
    /// if no adapter matches the selector or more than one does. Both of the
    /// latter name what the system actually has, because "the adapter you asked
    /// for is not the one you got" is the failure this constructor exists to
    /// make impossible.
    pub async fn with_config(config: MonitorConfig) -> Result<Self> {
        debug!("Initializing btleplug monitor");

        // Create manager
        let manager = btleplug::platform::Manager::new()
            .await
            .map_err(|e| Error::InitFailed(format!("Failed to create btleplug manager: {}", e)))?;

        // Get adapters using the Manager trait
        let adapters = manager
            .adapters()
            .await
            .map_err(|e| Error::InitFailed(format!("Failed to get adapters: {}", e)))?;

        let adapter = match config.adapter.as_deref() {
            None => adapters
                .into_iter()
                .next()
                .ok_or_else(|| Error::InitFailed("No Bluetooth adapters found".to_string()))?,
            Some(selector) => Self::select_adapter(adapters, selector).await?,
        };

        // Check if adapter is powered using Central trait
        let state = adapter
            .adapter_state()
            .await
            .map_err(|e| Error::InitFailed(format!("Failed to check adapter state: {}", e)))?;

        if state != btleplug::api::CentralState::PoweredOn {
            // Try to power on the adapter - note: btleplug doesn't have a direct power_on method
            // The user may need to power on the adapter manually
            warn!("Bluetooth adapter is not powered on. Please power it on manually.");
        }

        // The adapter's power state is worth knowing before the first event, not only
        // when a `StateUpdate` happens to arrive: a monitor built on an adapter that is
        // off should say so from the start.
        let health = Arc::new(match state {
            CentralState::PoweredOff => HealthState::unpowered(),
            _ => HealthState::new(),
        });

        debug!("btleplug monitor initialized successfully");

        Ok(Self {
            adapter: Arc::new(Mutex::new(adapter)),
            devices: Arc::new(DashMap::new()),
            scanning: Arc::new(AtomicBool::new(false)),
            scan_generation: Arc::new(AtomicU64::new(0)),
            scan_interval: config.scan_interval,
            events: DeviceEventSinks::new(),
            event_pump: Arc::new(Mutex::new(None)),
            notifications: NotificationHub::new(),
            notification_pumps: Arc::new(DashMap::new()),
            health,
            shutting_down: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Pick the one adapter the operator named out of the ones the system has.
    ///
    /// Descriptions are read first and matched afterwards so both failure
    /// messages can list what was actually found — an operator with a selector
    /// that matches nothing needs to see the ids to correct it.
    async fn select_adapter(
        adapters: Vec<btleplug::platform::Adapter>,
        selector: &str,
    ) -> Result<btleplug::platform::Adapter> {
        let mut matching = Vec::new();
        let mut found = Vec::new();

        for adapter in adapters {
            let description = adapter
                .adapter_info()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            found.push(description.clone());
            if adapter_matches(&description, selector) {
                matching.push((description, adapter));
            }
        }

        match matching.len() {
            1 => Ok(matching.pop().expect("one match counted").1),
            0 => Err(Error::InitFailed(format!(
                "no Bluetooth adapter matches '{selector}'; this system has: {}",
                found.join(", ")
            ))),
            count => Err(Error::InitFailed(format!(
                "'{selector}' matches {count} adapters ({}); select one of them exactly",
                matching
                    .into_iter()
                    .map(|(description, _)| description)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// Ask the adapter to start discovering, mapping the backend error.
    async fn arm_scan(adapter: &btleplug::platform::Adapter) -> Result<()> {
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to start scan: {}", e),
            })
    }

    /// Re-arm the controller-side scan every `scan_interval`.
    ///
    /// A scan that stops delivering reports is not an error anywhere: the
    /// adapter sleeps, `bluetoothd` restarts, or a stack lets its discovery
    /// session lapse, and the node keeps reporting a healthy scan while hearing
    /// nothing. Stop-then-start is what a stack that thinks it is still
    /// discovering needs — starting alone is a no-op there.
    ///
    /// The device cache is deliberately left alone: this refreshes the radio, it
    /// does not reset what has been seen. A [`MonitorConfig::scan_interval`] of
    /// zero turns the task off.
    fn spawn_scan_refresher(&self) {
        if self.scan_interval.is_zero() {
            return;
        }

        let scanning = self.scanning.clone();
        let generations = self.scan_generation.clone();
        let adapter = self.adapter.clone();
        let health = self.health.clone();
        let shutting_down = self.shutting_down.clone();
        let interval = self.scan_interval;
        let generation = generations.fetch_add(1, Ordering::SeqCst) + 1;

        tokio::spawn(async move {
            loop {
                time::sleep(interval).await;

                // Either the scan stopped, or a newer `start_scan` owns the
                // refresher slot now.
                if shutting_down.load(Ordering::SeqCst)
                    || !scanning.load(Ordering::SeqCst)
                    || generations.load(Ordering::SeqCst) != generation
                {
                    break;
                }

                let adapter = adapter.lock().await;
                if let Err(e) = adapter.stop_scan().await {
                    debug!("scan re-arm: stop_scan: {}", e);
                }
                health.note_rearm_attempt();
                match BtleplugMonitor::arm_scan(&adapter).await {
                    Ok(()) => {
                        // Recovery has to be as observable as the failure: this is what
                        // moves `is_scanning()` back to true after a run of failures.
                        health.note_rearm_success();
                        debug!("scan re-armed after {:?}", interval);
                    }
                    Err(e) => {
                        health.note_rearm_failure(&e.to_string());
                        warn!("failed to re-arm scan: {}", e);
                    }
                }
            }
        });
    }

    /// Convert btleplug peripheral ID to our DeviceId format.
    ///
    /// On BlueZ (Linux) a `PeripheralId` is the tail of a D-Bus object path and
    /// reads `hci0/dev_F4_FE_FB_CC_78_58` — a slash, a `dev_` marker, and
    /// underscores (`bluez_async::DeviceId`'s `Display`, which strips only the
    /// `/org/bluez/` prefix). The `hci0:` form this comment used to claim is not
    /// a thing btleplug emits; parsing against it left the adapter name inside
    /// every identifier a Linux node hashed, which is GAP_ANALYSIS M37.
    ///
    /// Normalisation lives on [`DeviceId::from_platform_id`] rather than here so
    /// that the crate which hashes the result can assert the same contract.
    fn peripheral_id_to_device_id(id: &btleplug::platform::PeripheralId) -> DeviceId {
        DeviceId::from_platform_id(&id.to_string())
    }

    /// Convert btleplug peripheral to our BluetoothDevice type.
    ///
    /// `services_resolved` comes from the caller — that is, from what this monitor has
    /// recorded — and never from the platform, because btleplug 0.12.0's `Peripheral`
    /// has no such query. See [`properties_to_device`].
    async fn peripheral_to_device(
        peripheral: &btleplug::platform::Peripheral,
        services_resolved: bool,
    ) -> Result<BluetoothDevice> {
        let id = Self::peripheral_id_to_device_id(&peripheral.id());

        let props = peripheral
            .properties()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to get properties: {}", e),
            })?;

        let is_connected = peripheral.is_connected().await.unwrap_or(false);

        Ok(properties_to_device(
            id,
            props,
            is_connected,
            services_resolved,
        ))
    }

    /// Convert btleplug CharPropFlags to our CharacteristicProperties.
    fn flags_to_props(flags: btleplug::api::CharPropFlags) -> CharacteristicProperties {
        CharacteristicProperties {
            broadcast: flags.contains(btleplug::api::CharPropFlags::BROADCAST),
            read: flags.contains(btleplug::api::CharPropFlags::READ),
            write_without_response: flags
                .contains(btleplug::api::CharPropFlags::WRITE_WITHOUT_RESPONSE),
            write: flags.contains(btleplug::api::CharPropFlags::WRITE),
            notify: flags.contains(btleplug::api::CharPropFlags::NOTIFY),
            indicate: flags.contains(btleplug::api::CharPropFlags::INDICATE),
            authenticated_signed_write: flags
                .contains(btleplug::api::CharPropFlags::AUTHENTICATED_SIGNED_WRITES),
            extended_properties: flags.contains(btleplug::api::CharPropFlags::EXTENDED_PROPERTIES),
        }
    }

    /// Convert btleplug Characteristic to our GattCharacteristic type.
    fn btleplug_char_to_gatt(char: &btleplug::api::Characteristic) -> GattCharacteristic {
        let char_uuid = CharacteristicUuid(char.uuid);
        let properties = Self::flags_to_props(char.properties);
        let handle = None; // btleplug doesn't expose handles directly

        GattCharacteristic {
            uuid: char_uuid,
            properties,
            handle,
        }
    }

    /// Convert btleplug Service to our GattService type.
    fn btleplug_service_to_gatt(service: &btleplug::api::Service) -> GattService {
        let svc_uuid = ServiceUuid(service.uuid);
        let is_primary = service.primary;

        // Convert characteristics
        let characteristics: Vec<GattCharacteristic> = service
            .characteristics
            .iter()
            .map(Self::btleplug_char_to_gatt)
            .collect();

        GattService {
            uuid: svc_uuid,
            is_primary,
            characteristics,
        }
    }

    /// Find a characteristic by UUID in the discovered services.
    fn find_characteristic(
        peripheral: &btleplug::platform::Peripheral,
        uuid: &CharacteristicUuid,
    ) -> Option<btleplug::api::Characteristic> {
        peripheral
            .characteristics()
            .into_iter()
            .find(|c| c.uuid == uuid.0)
    }
}

/// The snapshot a set of platform properties describes.
///
/// Split out from [`BtleplugMonitor::peripheral_to_device`] because everything in here is
/// a decision, and a decision nobody can test is a decision nobody notices going wrong:
/// this is where `services_resolved` used to be filled with `is_connected`, which is
/// TD-08 (GAP_ANALYSIS M31) — btleplug has no "are the services resolved" query, so the
/// only honest answer is the monitor's own record of whether `discover_services()` has
/// succeeded on this link. A connected device that has not been discovered reports
/// `false`, and the later `true` is then visible on the event stream instead of being
/// pre-empted by a field that was already true.
///
/// `raw_payload` stays `None` and `raw_payload_source` with it: btleplug hands over the
/// parsed advertisement properties and not the bytes they came from, so there is nothing
/// raw to pass on. The distinction is recorded in the field's doc, not papered over by
/// reconstructing a payload and letting a consumer read its silences as evidence.
fn properties_to_device(
    id: DeviceId,
    properties: Option<btleplug::api::PeripheralProperties>,
    is_connected: bool,
    services_resolved: bool,
) -> BluetoothDevice {
    let address = id.0.clone();

    let (name, rssi, manufacturer_data, service_data) = match properties {
        Some(props) => (
            props.local_name,
            props.rssi.map(|r| r as i32),
            props.manufacturer_data,
            props
                .service_data
                .into_iter()
                .map(|(uuid, bytes)| (ServiceUuid(uuid), bytes))
                .collect(),
        ),
        // No properties yet means the stack has seen the object, not what it said.
        // Empty maps rather than absent ones, and no invented payload.
        None => (None, None, HashMap::new(), HashMap::new()),
    };

    BluetoothDevice {
        id,
        address,
        name,
        rssi,
        is_connected,
        manufacturer_data,
        service_data,
        services_resolved,
        raw_payload: None,
        raw_payload_source: None,
    }
}

/// Map a btleplug failure from one GATT operation onto the variant a caller can act on.
///
/// The device, the call and the characteristic are all known here, which is exactly what
/// `Error::GattError` exists to carry — and what `BackendError` used to throw away for
/// every failure except a connect timeout (TD-12). The specific mappings are choices:
/// `NotConnected` is our `NotConnected` (the monitor knew the device, there was no link),
/// while btleplug's `DeviceNotFound` means the peripheral's object has gone, which is a
/// link that ended rather than one that was never made.
fn map_gatt_error(
    device_id: &DeviceId,
    operation: &str,
    uuid: Option<&CharacteristicUuid>,
    err: btleplug::Error,
) -> Error {
    let context = match uuid {
        Some(uuid) => format!("{operation} of characteristic {uuid}"),
        None => operation.to_string(),
    };

    match err {
        btleplug::Error::TimedOut(_) => Error::ConnectionTimeout,
        btleplug::Error::NotConnected => Error::NotConnected(device_id.clone()),
        btleplug::Error::DeviceNotFound => Error::Disconnected(device_id.clone()),
        btleplug::Error::NoSuchCharacteristic | btleplug::Error::UnexpectedCharacteristic => {
            match uuid {
                Some(uuid) => Error::CharacteristicNotFound(*uuid),
                None => Error::InvalidArgument(format!("{context}: {}", err)),
            }
        }
        btleplug::Error::Uuid(e) => Error::InvalidArgument(format!("Invalid UUID: {e}")),
        btleplug::Error::InvalidBDAddr(e) => {
            Error::InvalidArgument(format!("Invalid Bluetooth address: {e}"))
        }
        btleplug::Error::NoAdapterAvailable => Error::BackendUnavailable {
            required: BackendKind::Btleplug,
            available: BackendKind::compiled(),
        },
        btleplug::Error::NotSupported(message) => {
            Error::InvalidArgument(format!("{context} is not supported: {message}"))
        }
        other => Error::GattError {
            device_id: device_id.clone(),
            operation: context,
            message: other.to_string(),
        },
    }
}

/// One btleplug notification frame as this crate's value.
///
/// The frame names the service it arrived on, and that has to survive the trip: the
/// public stream is per device and carries every characteristic subscribed on the link,
/// so a consumer is left telling frames apart itself, and two services exposing the same
/// characteristic uuid are indistinguishable without this (TD-01).
fn frame_to_notification(frame: BtleplugNotification) -> ValueNotification {
    ValueNotification {
        characteristic: CharacteristicUuid(frame.uuid),
        service: Some(ServiceUuid(frame.service_uuid)),
        value: frame.value,
        timestamp: Some(std::time::Instant::now()),
    }
}

/// The device a link event describes, given what was cached before it.
///
/// Going down takes the resolved service table with it: the table belongs to the link, so
/// a device that reconnects has not had its services discovered until someone asks again.
/// Reporting both fields together is what lets a consumer see that the table it cached is
/// stale instead of reusing it across a reconnect (TD-08).
fn connection_report(previous: &BluetoothDevice, connected: bool) -> BluetoothDevice {
    let mut reported = previous.clone();
    reported.is_connected = connected;
    if !connected {
        reported.services_resolved = false;
    }
    reported
}

#[async_trait]
impl DeviceMonitor for BtleplugMonitor {
    async fn start_scan(&self) -> Result<()> {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Err(Error::ScanAlreadyInProgress);
        }

        debug!("Starting scan...");

        let adapter = self.adapter.lock().await;

        // Clear existing devices first
        self.devices.clear();

        // Start scanning
        if let Err(e) = Self::arm_scan(&adapter).await {
            self.scanning.store(false, Ordering::SeqCst);
            // Not counted as a re-arm failure: nothing was ever armed to re-arm.
            self.health.set_radio_ready(false);
            self.health.note_error(e.to_string());
            return Err(e);
        }
        self.health.set_radio_ready(true);

        info!("Scan started successfully");

        drop(adapter);

        // Give some time for initial discoveries
        time::sleep(Duration::from_millis(500)).await;

        // Get discovered peripherals using Central trait
        let adapter = self.adapter.lock().await;
        let peripherals = adapter
            .peripherals()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to get peripherals: {}", e),
            })?;

        for peripheral in peripherals {
            // A fresh cache means nothing has been discovered at the GATT level,
            // whatever the link state says.
            let device = Self::peripheral_to_device(&peripheral, false).await?;
            let id = device.id.clone();

            // Cache the device
            let peripheral_clone = peripheral.clone();
            self.devices.insert(
                id.clone(),
                DiscoveredDevice {
                    device: device.clone(),
                    peripheral: peripheral_clone,
                },
            );

            info!(
                "Discovered device: {} (name: {:?}, rssi: {:?})",
                id, device.name, device.rssi
            );
        }

        self.spawn_scan_refresher();

        Ok(())
    }

    async fn stop_scan(&self) -> Result<()> {
        if !self.scanning.load(Ordering::SeqCst) {
            return Err(Error::NotScanning);
        }

        debug!("Stopping scan...");

        let adapter = self.adapter.lock().await;
        adapter.stop_scan().await.map_err(|e| Error::BackendError {
            backend: BackendKind::Btleplug,
            message: format!("Failed to stop scan: {}", e),
        })?;

        self.scanning.store(false, Ordering::SeqCst);
        info!("Scan stopped successfully");

        Ok(())
    }

    async fn devices(&self) -> Result<Vec<BluetoothDevice>> {
        let devices: Vec<BluetoothDevice> = self
            .devices
            .iter()
            .map(|entry| entry.value().device.clone())
            .collect();

        debug!("Returning {} discovered devices", devices.len());
        Ok(devices)
    }

    async fn device(&self, id: &DeviceId) -> Result<BluetoothDevice> {
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        Ok(entry.value().device.clone())
    }

    async fn is_powered(&self) -> Result<bool> {
        let adapter = self.adapter.lock().await;
        let state = adapter
            .adapter_state()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to check adapter state: {}", e),
            })?;
        Ok(state == btleplug::api::CentralState::PoweredOn)
    }

    async fn adapter_info(&self) -> Result<String> {
        let adapter = self.adapter.lock().await;
        adapter
            .adapter_info()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to get adapter info: {}", e),
            })
    }

    async fn device_events(&self) -> Result<DeviceEventStream> {
        // One pump per monitor, however many consumers there are: each `adapter.events()`
        // is another D-Bus signal subscription, and two pumps would also mean the device
        // cache being written twice per report. Consumers join a fan-out registry that
        // counts what it had to drop, and a stream that is dropped releases its slot on
        // the spot rather than at the next radio report.
        self.start_event_pump().await;

        // A consumer that joins after the radio has been reporting would otherwise start
        // out blind: BlueZ synthesizes discoveries for the devices *it* knows, which
        // covers this stream's arrival but not the gap since the monitor's own first one.
        // The cached snapshot is replayed for that, and a duplicate `DeviceAdded` is a
        // normal part of this event model.
        let replay: Vec<DeviceEvent> = self
            .devices
            .iter()
            .map(|entry| DeviceEvent::DeviceAdded {
                device: entry.value().device.clone(),
            })
            .collect();

        Ok(self.events.subscribe_seeded(replay))
    }

    async fn is_scanning(&self) -> Result<bool> {
        // The radio's state, not the request: an adapter that is off or a scan that will
        // not re-arm is not a scan, and a node that kept saying otherwise could not tell
        // a dead radio from an empty room (TD-13).
        Ok(self.scanning.load(Ordering::SeqCst) && self.health.radio_ready())
    }

    fn health(&self) -> Option<MonitorHealth> {
        Some(self.health.snapshot(self.scanning.load(Ordering::SeqCst)))
    }
}

impl BtleplugMonitor {
    /// How many device events a consumer's channel could not hold.
    ///
    /// The channel is lossy on purpose — a radio pump must not queue behind a slow
    /// consumer — so this is the part that used to be missing: the number, not the
    /// silence. Logged at warn the first time and then every `CHANNEL_CAPACITY` after.
    pub fn dropped_events(&self) -> u64 {
        self.events.dropped_events()
    }

    /// Open the single platform event stream, once, and hand it to the shared fan-out.
    ///
    /// The lock is held only while the handle is checked and stored, never across the
    /// spawn, so two consumers arriving at once cannot end up with two pumps.
    async fn start_event_pump(&self) {
        let mut guard = self.event_pump.lock().await;
        if guard.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }

        let adapter = self.adapter.lock().await.clone();
        *guard = Some(tokio::spawn(Self::event_pump(
            adapter,
            self.devices.clone(),
            self.events.clone(),
            self.health.clone(),
            self.shutting_down.clone(),
        )));
    }
}

/// Turns the scan off when the monitor is discarded.
///
/// The refresher task holds `Arc` clones of the adapter and the scan flags, so
/// dropping the monitor is the only signal it has that nobody is listening any
/// more; without this it would keep re-arming discovery on an adapter whose
/// monitor is gone.
impl Drop for BtleplugMonitor {
    fn drop(&mut self) {
        self.scanning.store(false, Ordering::SeqCst);
        // The refresher, the event pump and the notification readers all hold `Arc`
        // clones of everything else they need, so this is their only stop signal. Without
        // it a discarded monitor keeps re-arming discovery on an adapter nobody owns, and
        // keeps its D-Bus subscriptions with it.
        self.shutting_down.store(true, Ordering::SeqCst);

        // `retain` rather than iterating and removing: removing a key while an
        // `iter_mut()` guard is held deadlocks that shard.
        self.notification_pumps.retain(|_, task| {
            task.abort();
            false
        });

        // Anyone still holding a notification stream is told the device is gone rather
        // than waiting for a frame from a monitor that no longer exists.
        self.notifications.forget_all();
    }
}

impl BtleplugMonitor {
    /// Feed the shared fan-out from the platform's central event stream.
    ///
    /// Three things this has to survive that the old per-consumer pump did not:
    ///
    /// - **Nobody listening.** The pump parks while the registry is empty and releases
    ///   the platform stream when the last consumer goes, so a monitor that stopped being
    ///   read stops costing a D-Bus subscription — and does not notice that a consumer
    ///   left only when it next has something to say, which is how an idle pump lingered.
    /// - **The stream ending.** A `bluetoothd` restart, an adapter reset, a D-Bus
    ///   disconnect all end it. That used to be the consumer's problem; reopening is the
    ///   monitor's, since the alternative is a node that scans forever and hears nothing.
    /// - **A consumer that cannot keep up.** Counted by [`DeviceEventSinks::publish`]
    ///   rather than left to a debug line.
    async fn event_pump(
        adapter: btleplug::platform::Adapter,
        devices: Arc<DashMap<DeviceId, DiscoveredDevice>>,
        sinks: DeviceEventSinks,
        health: Arc<HealthState>,
        shutting_down: Arc<AtomicBool>,
    ) {
        while !shutting_down.load(Ordering::SeqCst) {
            sinks.wait_until_subscribed().await;
            if shutting_down.load(Ordering::SeqCst) {
                return;
            }

            let central_events = match adapter.events().await {
                Ok(stream) => {
                    health.note_event_stream_open();
                    stream
                }
                Err(e) => {
                    health.note_error(format!("opening the adapter event stream: {e}"));
                    warn!("Failed to open btleplug central event stream: {}", e);
                    time::sleep(EVENT_STREAM_RETRY).await;
                    continue;
                }
            };
            futures::pin_mut!(central_events);

            loop {
                if shutting_down.load(Ordering::SeqCst) {
                    return;
                }

                tokio::select! {
                    biased;
                    _ = sinks.wait_until_empty() => {
                        info!(
                            "every device-event consumer is gone; releasing the platform \
                             event stream until one returns"
                        );
                        break;
                    }
                    event = central_events.next() => {
                        let Some(event) = event else {
                            debug!(
                                "btleplug central event stream ended; reopening it after {:?} \
                                 if a consumer is still reading",
                                EVENT_STREAM_RETRY
                            );
                            break;
                        };

                        for mapped in
                            Self::map_central_event(&adapter, &devices, &health, event).await
                        {
                            sinks.publish(&mapped);
                        }
                    }
                }
            }

            // Only back off if the stream ended on us; breaking because the audience left
            // should be able to restart the moment somebody asks again.
            if sinks.subscriber_count() > 0 {
                time::sleep(EVENT_STREAM_RETRY).await;
            }
        }
    }

    /// Map a btleplug [`CentralEvent`] to zero or more [`DeviceEvent`]s,
    /// keeping the device cache current along the way.
    async fn map_central_event(
        adapter: &btleplug::platform::Adapter,
        devices: &DashMap<DeviceId, DiscoveredDevice>,
        health: &HealthState,
        event: CentralEvent,
    ) -> Vec<DeviceEvent> {
        match event {
            // A discovery means the app should record an occurrence for the
            // device. The consumer rate-limits duplicates, so emitting
            // DeviceAdded even for already-cached devices (BlueZ synthesizes
            // initial discoveries when the stream opens) is intentional.
            CentralEvent::DeviceDiscovered(id) => {
                match Self::fetch_and_cache_device(adapter, devices, &id).await {
                    Some(device) => vec![DeviceEvent::DeviceAdded { device }],
                    None => Vec::new(),
                }
            }

            CentralEvent::DeviceUpdated(id) => {
                let device_id = Self::peripheral_id_to_device_id(&id);
                let old = devices.get(&device_id).map(|e| e.value().device.clone());
                match Self::fetch_and_cache_device(adapter, devices, &id).await {
                    Some(device) => match old {
                        None => vec![DeviceEvent::DeviceAdded { device }],
                        Some(old) => vec![report_event(&old, device)],
                    },
                    None => {
                        // The device is no longer known to the adapter
                        // (e.g. the Bluetooth stack was reset).
                        devices.remove(&device_id);
                        vec![DeviceEvent::DeviceRemoved { id: device_id }]
                    }
                }
            }

            // btleplug asks BlueZ for `duplicate_data`, so an RSSI report arrives
            // with every advertisement — including for a device that has not moved
            // and whose signal strength has not budged. It used to be dropped when
            // the number was unchanged, which is how a beacon held next to the node
            // came to produce one sighting and then silence (GAP_ANALYSIS B14).
            CentralEvent::RssiUpdate { id, rssi } => {
                Self::report_advertisement(adapter, devices, &id, |device| {
                    device.rssi = Some(rssi as i32);
                })
                .await
            }

            // A link this node did not ask for — a peripheral that reconnected on its
            // own, or a connect issued through `bluetoothctl` — arrives here, and it is
            // reported through the same path as the one `connect()` takes (TD-15).
            CentralEvent::DeviceConnected(id) => {
                let device_id = Self::peripheral_id_to_device_id(&id);
                Self::update_connected(devices, &device_id, true)
                    .into_iter()
                    .collect()
            }
            CentralEvent::DeviceDisconnected(id) => {
                let device_id = Self::peripheral_id_to_device_id(&id);
                Self::update_connected(devices, &device_id, false)
                    .into_iter()
                    .collect()
            }

            CentralEvent::ManufacturerDataAdvertisement {
                id,
                manufacturer_data,
            } => {
                Self::report_advertisement(adapter, devices, &id, |device| {
                    device.manufacturer_data = manufacturer_data;
                })
                .await
            }

            CentralEvent::ServiceDataAdvertisement { id, service_data } => {
                Self::report_advertisement(adapter, devices, &id, |device| {
                    device.service_data = service_data
                        .into_iter()
                        .map(|(uuid, data)| (ServiceUuid(uuid), data))
                        .collect();
                })
                .await
            }

            // The advertised service *list* has no field on `BluetoothDevice` to
            // land in, so this report cannot move any tracked property
            // (GAP_ANALYSIS M27, and M31 for why `services_resolved` is not it).
            // It is still the device being heard, which is worth more than the
            // debug line it used to end at.
            CentralEvent::ServicesAdvertisement { id, .. } => {
                Self::report_advertisement(adapter, devices, &id, |_| {}).await
            }

            CentralEvent::DeviceServicesModified(id) => {
                Self::report_advertisement(adapter, devices, &id, |_| {}).await
            }

            CentralEvent::StateUpdate(state) => {
                // The radio itself changing state is not a report about a device, so it
                // has no place in this stream — but it used to have no place anywhere,
                // which is how an adapter toggled off looked exactly like an empty room.
                // It goes to the health state instead, where a consumer polling every
                // minute can still see it (`is_scanning()` follows suit).
                match state {
                    CentralState::PoweredOn => {
                        health.set_adapter_powered(true);
                        info!("Bluetooth adapter powered on");
                    }
                    CentralState::PoweredOff => {
                        health.set_adapter_powered(false);
                        health.note_error("the Bluetooth adapter reported itself powered off");
                        warn!(
                            "Bluetooth adapter powered off; is_scanning() is now false and \
                             device events will pause until it powers back on"
                        );
                    }
                    CentralState::Unknown => {
                        debug!("Bluetooth adapter state unknown");
                    }
                }
                Vec::new()
            }
        }
    }

    /// Update the cached connection state of a device, reporting the transition through
    /// [`report_event`] like any other change — nothing at all when nothing moved.
    ///
    /// Both the platform's `DeviceConnected`/`DeviceDisconnected` and this node's own
    /// `connect()`/`disconnect()` arrive here, so whoever gets there first is the one that
    /// says so and the other is silent rather than a duplicate report.
    fn update_connected(
        devices: &DashMap<DeviceId, DiscoveredDevice>,
        device_id: &DeviceId,
        connected: bool,
    ) -> Option<DeviceEvent> {
        let mut entry = devices.get_mut(device_id)?;
        let previous = entry.value().device.clone();
        let reported = connection_report(&previous, connected);
        entry.value_mut().device = reported.clone();
        Some(report_event(&previous, reported))
    }

    /// Starts the one notification reader this device needs, if it is not running.
    ///
    /// btleplug gives each peripheral a single notification stream whose items go to
    /// whichever reader wakes up, so reading it more than once loses frames. The reader is
    /// therefore per device and refcounted by the hub: `subscribe()` and every
    /// `notifications()` call share it, and N calls do not mean N platform streams.
    ///
    /// It ends on its own when the last consumer drops (nobody is left to deliver to) or
    /// when [`stop_notification_pump`] sees the link go; either way the next call starts a
    /// fresh one.
    fn ensure_notification_pump(
        &self,
        device_id: &DeviceId,
        peripheral: &btleplug::platform::Peripheral,
    ) {
        if let Some(existing) = self.notification_pumps.get(device_id) {
            if !existing.is_finished() {
                return;
            }
            // A finished handle can never deliver again, so replacing it is the only way
            // this device can have a reader.
        }

        let hub = self.notifications.clone();
        let peripheral = peripheral.clone();
        let id = device_id.clone();
        let pumps = self.notification_pumps.clone();
        let health = self.health.clone();

        let handle = tokio::spawn(async move {
            debug!("Starting notification reader for device: {}", id);
            let mut frames = match peripheral.notifications().await {
                Ok(frames) => frames,
                Err(e) => {
                    // The subscription is written but the frames have nowhere to come
                    // from: say so rather than leave consumers reading a stream that is
                    // open and will never deliver.
                    warn!(
                        "Cannot read notifications for {}: {}; the streams opened on it \
                         end rather than hang",
                        id, e
                    );
                    health.note_error(format!("notification stream for {id} unavailable: {e}"));
                    hub.forget(&id);
                    pumps.remove(&id);
                    return;
                }
            };
            let mut delivered = 0u64;
            let mut no_consumers = false;

            loop {
                tokio::select! {
                    biased;
                    // Nobody left to hand frames to: stop reading, which also stops the
                    // CCCDs keeping the peripheral's notifications on for a node that is
                    // not listening.
                    _ = hub.wait_until_empty(&id) => {
                        no_consumers = true;
                        break;
                    }
                    frame = frames.next() => {
                        let Some(frame) = frame else { break };
                        delivered += 1;
                        hub.publish(&id, frame_to_notification(frame));
                    }
                }
            }

            debug!(
                "Notification reader for {} ended after {} frame(s)",
                id, delivered
            );
            if !no_consumers {
                // The platform stream ending means the link or the stack went: this node
                // stopped hearing from a peripheral it was subscribed to, which is exactly
                // what a consumer watching a stalled stream cannot tell by itself. The
                // next subscribe starts a fresh reader.
                health.note_error(format!(
                    "notification stream for {id} ended after {delivered} frame(s)"
                ));
            }
            hub.forget(&id);
            pumps.remove(&id);
        });

        if let Some(previous) = self.notification_pumps.insert(device_id.clone(), handle) {
            // Only reachable if two callers saw no live reader at the same moment; one of
            // them wins and the other's reader would double-read the platform stream.
            previous.abort();
        }
    }

    /// Stops a device's notification reader and ends every stream opened on it.
    fn stop_notification_pump(&self, device_id: &DeviceId) {
        if let Some((_, handle)) = self.notification_pumps.remove(device_id) {
            handle.abort();
        }
        self.notifications.forget(device_id);
    }

    /// Look up a peripheral (cache first, then the adapter), convert it to a
    /// [`BluetoothDevice`], and refresh the cache entry.
    ///
    /// `services_resolved` is carried forward from the cache rather than re-derived,
    /// because nothing on the platform reports it: the monitor's own record of whether
    /// `discover_services()` has run on this link is the only answer there is, and a
    /// refresh that recomputed it from `is_connected` would silently un-resolve a device
    /// on every advertisement.
    async fn fetch_and_cache_device(
        adapter: &btleplug::platform::Adapter,
        devices: &DashMap<DeviceId, DiscoveredDevice>,
        id: &btleplug::platform::PeripheralId,
    ) -> Option<BluetoothDevice> {
        let device_id = Self::peripheral_id_to_device_id(id);
        let (peripheral, services_resolved) = match devices.get(&device_id) {
            Some(entry) => {
                let peripheral = entry.value().peripheral.clone();
                let services_resolved = entry.value().device.services_resolved;
                drop(entry);
                (peripheral, services_resolved)
            }
            None => match adapter.peripheral(id).await {
                Ok(peripheral) => (peripheral, false),
                Err(e) => {
                    debug!("Failed to get peripheral {}: {}", device_id, e);
                    return None;
                }
            },
        };

        let device = match Self::peripheral_to_device(&peripheral, services_resolved).await {
            Ok(device) => device,
            Err(e) => {
                debug!("Failed to read properties for {}: {}", device_id, e);
                return None;
            }
        };

        let id = device.id.clone();
        devices.insert(
            id.clone(),
            DiscoveredDevice {
                device: device.clone(),
                peripheral,
            },
        );
        Some(device)
    }

    /// Deliver one advertisement report about a device the cache already holds,
    /// or turn it into a discovery when the cache has never seen that device.
    ///
    /// `apply` writes the contents this report carried onto the cached snapshot.
    /// The report always produces an event: [`report_event`] names what moved,
    /// and reports the device as heard again when nothing did. Returning nothing
    /// for an unchanged report is what made a stationary beacon produce one
    /// sighting and then silence (GAP_ANALYSIS B14), because "no event" is
    /// indistinguishable at the consumer from "the device left".
    ///
    /// An advertisement from a device the monitor never saw is the only evidence
    /// of it there is, so it is fetched and reported as a discovery — the same
    /// reasoning the RSSI arm applies to an unknown device.
    async fn report_advertisement<F>(
        adapter: &btleplug::platform::Adapter,
        devices: &DashMap<DeviceId, DiscoveredDevice>,
        id: &btleplug::platform::PeripheralId,
        apply: F,
    ) -> Vec<DeviceEvent>
    where
        F: FnOnce(&mut BluetoothDevice),
    {
        let device_id = Self::peripheral_id_to_device_id(id);

        if let Some(mut entry) = devices.get_mut(&device_id) {
            let old = entry.value().device.clone();
            let peripheral = entry.value().peripheral.clone();
            let mut reported = old.clone();
            apply(&mut reported);
            let event = report_event(&old, reported.clone());
            *entry.value_mut() = DiscoveredDevice {
                device: reported,
                peripheral,
            };
            return vec![event];
        }

        match Self::fetch_and_cache_device(adapter, devices, id).await {
            Some(device) => {
                debug!(
                    "Advertisement from unknown device {} became a discovery",
                    device_id
                );
                vec![DeviceEvent::DeviceAdded { device }]
            }
            None => {
                debug!("Advertisement from {} could not be resolved", device_id);
                Vec::new()
            }
        }
    }
}

#[async_trait]
impl GattClient for BtleplugMonitor {
    async fn connect(&self, id: &DeviceId) -> Result<()> {
        debug!("Connecting to device: {}", id);

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();

        drop(entry);

        // Connect with timeout using the trait method
        peripheral
            .connect_with_timeout(Duration::from_secs(10))
            .await
            .map_err(|e| map_gatt_error(&device_id, "connect", None, e))?;

        info!("Connected to device: {}", device_id);

        // The link this call made is reported exactly like one the radio made: the
        // platform will report the same moment through `DeviceConnected`, and whichever
        // arrives second sees no field move, so it reaches consumers as an
        // `Advertisement` rather than a second `DeviceUpdated`. A consumer driving off the
        // stream rather than polling therefore learns about its own connect, which it
        // could not before (TD-15).
        if let Some(event) = Self::update_connected(&self.devices, &device_id, true) {
            self.events.publish(&event);
        }

        Ok(())
    }

    async fn disconnect(&self, id: &DeviceId) -> Result<()> {
        debug!("Disconnecting from device: {}", id);

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();

        drop(entry);

        peripheral
            .disconnect()
            .await
            .map_err(|e| map_gatt_error(&device_id, "disconnect", None, e))?;

        // The subscriptions died with the link: the CCCDs were the link's, so the reader
        // stops and every open notification stream ends. A caller that reconnects has to
        // subscribe again, exactly as on a real peripheral.
        if let Some(event) = Self::update_connected(&self.devices, &device_id, false) {
            self.events.publish(&event);
        }
        self.stop_notification_pump(&device_id);

        info!("Disconnected from device: {}", device_id);
        Ok(())
    }

    async fn is_connected(&self, id: &DeviceId) -> Result<bool> {
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        entry
            .value()
            .peripheral
            .is_connected()
            .await
            .map_err(|e| map_gatt_error(id, "is_connected", None, e))
    }

    async fn discover_services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        debug!("Discovering services for device: {}", id);

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();

        drop(entry);

        if !peripheral.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        // Discover services
        peripheral
            .discover_services()
            .await
            .map_err(|e| map_gatt_error(&device_id, "discover_services", None, e))?;

        // Get services from peripheral
        let services = peripheral.services();

        // Convert to our GattService type
        let gatt_services: Vec<GattService> = services
            .iter()
            .map(Self::btleplug_service_to_gatt)
            .collect();

        // Discovery is the only thing that can make `services_resolved` true, and the
        // transition is worth reporting: a consumer that has been waiting for a GATT
        // table now knows to stop. BlueZ resolves this on connect, which is why reading
        // the flag off the platform would have said true a moment too early (TD-08).
        if let Some(mut entry) = self.devices.get_mut(&device_id) {
            let previous = entry.value().device.clone();
            entry.value_mut().device.services_resolved = true;
            let event = report_event(&previous, entry.value().device.clone());
            drop(entry);
            self.events.publish(&event);
        }

        info!(
            "Discovered {} services for device: {}",
            gatt_services.len(),
            device_id
        );
        Ok(gatt_services)
    }

    async fn services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        drop(entry);

        let services = peripheral.services();
        let gatt_services: Vec<GattService> = services
            .iter()
            .map(Self::btleplug_service_to_gatt)
            .collect();

        Ok(gatt_services)
    }

    async fn read_characteristic(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
    ) -> Result<Vec<u8>> {
        debug!("Reading characteristic {} from device {}", uuid, id);

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();

        drop(entry);

        if !peripheral.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        // Find the characteristic
        let characteristic = Self::find_characteristic(&peripheral, uuid)
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        // Read the characteristic using the trait method
        let value = peripheral
            .read(&characteristic)
            .await
            .map_err(|e| map_gatt_error(&device_id, "read", Some(uuid), e))?;

        debug!("Read {} bytes from characteristic {}", value.len(), uuid);
        Ok(value)
    }

    async fn write_characteristic(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
        value: &[u8],
        response: bool,
    ) -> Result<()> {
        debug!("Writing to characteristic {} on device {}", uuid, id);

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();

        drop(entry);

        if !peripheral.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        // Find the characteristic
        let characteristic = Self::find_characteristic(&peripheral, uuid)
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        // Determine write type
        let write_type = if response {
            WriteType::WithResponse
        } else {
            WriteType::WithoutResponse
        };

        // Write the characteristic using the trait method
        peripheral
            .write(&characteristic, value, write_type)
            .await
            .map_err(|e| map_gatt_error(&device_id, "write", Some(uuid), e))?;

        debug!(
            "Successfully wrote {} bytes to characteristic {}",
            value.len(),
            uuid
        );
        Ok(())
    }

    async fn subscribe(&self, id: &DeviceId, uuid: &CharacteristicUuid) -> Result<()> {
        debug!(
            "Subscribing to notifications for characteristic {} on device {}",
            uuid, id
        );

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();

        drop(entry);

        if !peripheral.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        // Find the characteristic
        let characteristic = Self::find_characteristic(&peripheral, uuid)
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        // Subscribe using the trait method
        peripheral
            .subscribe(&characteristic)
            .await
            .map_err(|e| map_gatt_error(&device_id, "subscribe", Some(uuid), e))?;

        // The CCCD is written, so frames are coming: start the one reader this device
        // needs if it is not already running. `notifications()` does the same, and either
        // order works — a caller that subscribes first gets the frames on the stream it
        // opens afterwards, and a caller that opens the stream first does not lose the
        // frames between the two calls.
        self.ensure_notification_pump(&device_id, &peripheral);

        debug!("Successfully subscribed to characteristic {}", uuid);
        Ok(())
    }

    async fn unsubscribe(&self, id: &DeviceId, uuid: &CharacteristicUuid) -> Result<()> {
        debug!(
            "Unsubscribing from notifications for characteristic {} on device {}",
            uuid, id
        );

        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();

        drop(entry);

        if !peripheral.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        // Find the characteristic
        let characteristic = Self::find_characteristic(&peripheral, uuid)
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        // Unsubscribe using the trait method
        peripheral
            .unsubscribe(&characteristic)
            .await
            .map_err(|e| map_gatt_error(&device_id, "unsubscribe", Some(uuid), e))?;

        debug!("Successfully unsubscribed from characteristic {}", uuid);
        Ok(())
    }

    /// Every frame this device sends, fanned out to every caller.
    ///
    /// btleplug exposes notifications per *peripheral* on one stream that hands each frame
    /// to a single reader, so this cannot hand out the platform stream directly: two
    /// callers would split the frames between them, each seeing an arbitrary subset with
    /// nothing to say it had. Instead the monitor reads the platform stream once (see
    /// [`NotificationHub`]) and this returns a view of that.
    ///
    /// The reader starts on the first call for a device and stops when the last consumer
    /// goes away or the link drops. Frames published before this call are not replayed —
    /// a subscription is a promise about what comes next, not what already happened.
    async fn notifications(&self, id: &DeviceId) -> Result<NotificationStream> {
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();
        drop(entry);

        self.ensure_notification_pump(&device_id, &peripheral);
        Ok(self.notifications.subscribe(&device_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::events::UpdateField;

    #[test]
    fn test_flags_to_props() {
        let flags = btleplug::api::CharPropFlags::READ | btleplug::api::CharPropFlags::NOTIFY;
        let props = BtleplugMonitor::flags_to_props(flags);

        assert!(props.read);
        assert!(props.notify);
        assert!(!props.write);
        assert!(!props.broadcast);
    }

    #[test]
    fn test_device_id_from_string() {
        let id: DeviceId = "AA:BB:CC:DD:EE:FF".to_string().into();
        assert_eq!(id.0, "AA:BB:CC:DD:EE:FF");
    }

    fn properties() -> btleplug::api::PeripheralProperties {
        btleplug::api::PeripheralProperties {
            local_name: Some("beacon-7".to_string()),
            rssi: Some(-63),
            manufacturer_data: HashMap::from([(0x004C, vec![0x02, 0x15])]),
            service_data: HashMap::from([(uuid::Uuid::nil(), vec![0xAA])]),
            ..Default::default()
        }
    }

    #[test]
    fn a_snapshot_reports_only_what_the_platform_actually_said() {
        let device = properties_to_device(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            Some(properties()),
            true,
            false,
        );

        assert_eq!(device.name.as_deref(), Some("beacon-7"));
        assert_eq!(device.rssi, Some(-63));
        assert_eq!(
            device.manufacturer_data.get(&0x004C),
            Some(&vec![0x02, 0x15])
        );
        assert_eq!(
            device.service_data.get(&ServiceUuid(uuid::Uuid::nil())),
            Some(&vec![0xAA])
        );
        assert!(
            device.raw_payload.is_none() && device.raw_payload_source.is_none(),
            "btleplug hands over parsed properties, not the bytes they were parsed from, so \
             there is nothing raw to report and nothing to label — a payload invented here \
             would let a consumer read its silences as evidence that an AD structure was \
             absent"
        );
    }

    #[test]
    fn resolution_follows_discovery_and_not_the_link() {
        // TD-08: the flag used to be filled with `is_connected`, so every connected device
        // looked like its GATT table was ready and the moment discovery actually resolved
        // it was invisible.
        let connected = properties_to_device(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            Some(properties()),
            true,
            false,
        );
        assert!(!connected.services_resolved, "connected is not resolved");

        let resolved = properties_to_device(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            Some(properties()),
            true,
            true,
        );
        assert!(resolved.services_resolved);

        let unseen = properties_to_device(DeviceId::new("AA:BB:CC:DD:EE:FF"), None, false, false);
        assert!(
            unseen.manufacturer_data.is_empty() && unseen.service_data.is_empty(),
            "no properties is not the same as an empty advertisement, but it cannot be \
             anything but empty maps in this struct"
        );
        assert!(unseen.name.is_none());
    }

    #[test]
    fn a_link_going_down_takes_the_resolved_table_with_it() {
        let resolved = properties_to_device(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            Some(properties()),
            true,
            true,
        );

        let down = connection_report(&resolved, false);
        assert!(!down.is_connected);
        assert!(
            !down.services_resolved,
            "the service table belongs to the link, so a reconnect has not been discovered \
             until someone asks again"
        );

        // The report that goes out over that transition names both fields, which is how a
        // consumer knows the table it cached is stale.
        let event = report_event(&resolved, down.clone());
        match event {
            DeviceEvent::DeviceUpdated { changed_fields, .. } => {
                assert_eq!(
                    changed_fields,
                    vec![UpdateField::ServicesResolved, UpdateField::Connected],
                    "declaration order, and both of them"
                );
            }
            other => panic!("a link going down is a change, not an advertisement: {other:?}"),
        }

        let up = connection_report(&down, true);
        assert!(up.is_connected);
        assert!(
            !up.services_resolved,
            "coming back up does not re-resolve anything"
        );
    }

    #[test]
    fn a_platform_failure_becomes_the_variant_a_caller_can_act_on() {
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        let uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();

        assert!(matches!(
            map_gatt_error(
                &id,
                "connect",
                None,
                btleplug::Error::TimedOut(Duration::from_secs(10))
            ),
            Error::ConnectionTimeout
        ));
        assert!(matches!(
            map_gatt_error(&id, "read", Some(&uuid), btleplug::Error::NotConnected),
            Error::NotConnected(known) if known == id
        ));
        assert!(
            matches!(
                map_gatt_error(&id, "write", Some(&uuid), btleplug::Error::DeviceNotFound),
                Error::Disconnected(gone) if gone == id
            ),
            "the peripheral's object disappearing is a link that ended, not one that was \
             never made"
        );
        assert!(matches!(
            map_gatt_error(
                &id,
                "subscribe",
                Some(&uuid),
                btleplug::Error::NoSuchCharacteristic
            ),
            Error::CharacteristicNotFound(missing) if missing == uuid
        ));
        assert!(matches!(
            map_gatt_error(&id, "start_scan", None, btleplug::Error::NoAdapterAvailable),
            Error::BackendUnavailable { required: BackendKind::Btleplug, available }
                if !available.is_empty()
        ));

        // Everything else keeps the device, the call and the characteristic, which is what
        // `GattError` is for and what `BackendError` used to throw away.
        let error = map_gatt_error(
            &id,
            "read",
            Some(&uuid),
            btleplug::Error::Other(" ATT_ERROR_READ_NOT_PERMITTED ".into()),
        );
        match error {
            Error::GattError {
                device_id,
                operation,
                message,
            } => {
                assert_eq!(device_id, id);
                assert_eq!(operation, format!("read of characteristic {uuid}"));
                assert!(message.contains("READ_NOT_PERMITTED"), "{message}");
            }
            other => panic!("a GATT-layer failure has to say which call failed: {other:?}"),
        }
    }

    #[test]
    fn a_frame_keeps_the_service_it_arrived_on() {
        // The public stream is per device, so frames from every characteristic subscribed
        // on the link share it: without the service there is no way to tell two same-uuid
        // characteristics in different services apart.
        let frame = BtleplugNotification {
            uuid: uuid::Uuid::nil(),
            service_uuid: uuid::Uuid::from_u128(0x180f),
            value: vec![0x64],
        };

        let notification = frame_to_notification(frame);
        assert_eq!(notification.characteristic.0, uuid::Uuid::nil());
        assert_eq!(
            notification.service,
            Some(ServiceUuid(uuid::Uuid::from_u128(0x180f)))
        );
        assert_eq!(notification.value, vec![0x64]);
        assert!(notification.timestamp.is_some());
    }
}
