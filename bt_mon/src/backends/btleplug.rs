//! btleplug backend implementation for bt_mon.
//!
//! This module provides the cross-platform implementation using the `btleplug` crate.

use async_trait::async_trait;
use btleplug::api::{
    Central as _, CentralEvent, CentralState, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use dashmap::DashMap;
use futures::stream::{self, StreamExt};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time;

use crate::config::{adapter_matches, MonitorConfig};
use crate::error::{BackendKind, Error, Result};
use crate::monitor::events::{
    report_event, DeviceEvent, DeviceEventStream, NotificationStream, UpdateField,
    DEVICE_EVENT_CHANNEL_CAPACITY,
};
use crate::monitor::{DeviceMonitor, GattClient};
use crate::types::ValueNotification;
use crate::types::{
    BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId, GattCharacteristic,
    GattService, ServiceUuid,
};

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
    scanning: Arc<AtomicBool>,
    /// Bumped by every `start_scan`, so the refresher task belonging to an
    /// earlier scan exits instead of leaving one running per restart.
    scan_generation: Arc<AtomicU64>,
    /// How often the running scan is re-armed; see [`MonitorConfig::scan_interval`].
    scan_interval: Duration,
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

        debug!("btleplug monitor initialized successfully");

        Ok(Self {
            adapter: Arc::new(Mutex::new(adapter)),
            devices: Arc::new(DashMap::new()),
            scanning: Arc::new(AtomicBool::new(false)),
            scan_generation: Arc::new(AtomicU64::new(0)),
            scan_interval: config.scan_interval,
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
        let interval = self.scan_interval;
        let generation = generations.fetch_add(1, Ordering::SeqCst) + 1;

        tokio::spawn(async move {
            loop {
                time::sleep(interval).await;

                // Either the scan stopped, or a newer `start_scan` owns the
                // refresher slot now.
                if !scanning.load(Ordering::SeqCst)
                    || generations.load(Ordering::SeqCst) != generation
                {
                    break;
                }

                let adapter = adapter.lock().await;
                if let Err(e) = adapter.stop_scan().await {
                    debug!("scan re-arm: stop_scan: {}", e);
                }
                match BtleplugMonitor::arm_scan(&adapter).await {
                    Ok(()) => debug!("scan re-armed after {:?}", interval),
                    Err(e) => warn!("failed to re-arm scan: {}", e),
                }
            }
        });
    }

    /// Convert btleplug peripheral ID to our DeviceId format.
    ///
    /// On BlueZ (Linux) peripheral IDs are D-Bus object path segments of the
    /// form `hci0:AA:BB:CC:DD:EE:FF`. The adapter prefix is stripped so IDs
    /// are bare MAC addresses, consistent with the other backends and with
    /// the app's MAC parsing.
    fn peripheral_id_to_device_id(id: &btleplug::platform::PeripheralId) -> DeviceId {
        let raw = format!("{}", id);
        let mac = raw.split_once(':').and_then(|(prefix, rest)| {
            if prefix.starts_with("hci")
                && prefix["hci".len()..].chars().all(|c| c.is_ascii_digit())
            {
                Some(rest)
            } else {
                None
            }
        });
        mac.unwrap_or(&raw).to_string().into()
    }

    /// Convert btleplug peripheral to our BluetoothDevice type.
    async fn peripheral_to_device(
        peripheral: &btleplug::platform::Peripheral,
    ) -> Result<BluetoothDevice> {
        let id = Self::peripheral_id_to_device_id(&peripheral.id());
        let address = id.0.clone();

        // Get properties from the peripheral
        let props = peripheral
            .properties()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to get properties: {}", e),
            })?;

        let (name, rssi, manufacturer_data, service_data) = if let Some(props) = props {
            let name = props.local_name;
            let rssi = props.rssi.map(|r| r as i32);

            // Convert manufacturer data
            let manufacturer_data = props.manufacturer_data;

            // Convert service data - btleplug uses Uuid, we need ServiceUuid
            let service_data: HashMap<ServiceUuid, Vec<u8>> = props
                .service_data
                .into_iter()
                .map(|(uuid, bytes)| (ServiceUuid(uuid), bytes))
                .collect();

            (name, rssi, manufacturer_data, service_data)
        } else {
            (None, None, HashMap::new(), HashMap::new())
        };

        // Check connection state
        let is_connected = peripheral.is_connected().await.unwrap_or(false);

        Ok(BluetoothDevice {
            id,
            address,
            name,
            rssi,
            is_connected,
            manufacturer_data,
            service_data,
            services_resolved: is_connected,
            // btleplug hands over the parsed advertisement properties and not
            // the bytes they came from, so there is nothing raw to pass on.
            raw_payload: None,
        })
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
            return Err(e);
        }

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
            let device = Self::peripheral_to_device(&peripheral).await?;
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
        // The btleplug central event stream ends when the adapter's event
        // source goes away (D-Bus disconnect, bluetoothd restart, adapter
        // reset). Each call to this method yields one fresh stream; consumers
        // (see FullNode::run) are expected to reopen it after a delay when it
        // closes. On BlueZ, opening the stream also synthesizes
        // DeviceDiscovered events for devices already known to the daemon.
        let adapter = self.adapter.lock().await.clone();
        let devices = self.devices.clone();
        let (tx, rx) = futures::channel::mpsc::channel(DEVICE_EVENT_CHANNEL_CAPACITY);

        tokio::spawn(Self::event_pump(adapter, devices, tx));

        Ok(Box::pin(rx))
    }

    async fn is_scanning(&self) -> Result<bool> {
        Ok(self.scanning.load(Ordering::SeqCst))
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
    }
}

impl BtleplugMonitor {
    /// Consume the btleplug central event stream, mapping each [`CentralEvent`]
    /// to [`DeviceEvent`]s and forwarding them over `tx`.
    ///
    /// The task exits when the central event stream ends (the returned
    /// stream then completes) or when the receiver is dropped.
    async fn event_pump(
        adapter: btleplug::platform::Adapter,
        devices: Arc<DashMap<DeviceId, DiscoveredDevice>>,
        mut tx: futures::channel::mpsc::Sender<DeviceEvent>,
    ) {
        let central_events = match adapter.events().await {
            Ok(stream) => stream,
            Err(e) => {
                warn!("Failed to open btleplug central event stream: {}", e);
                return;
            }
        };
        futures::pin_mut!(central_events);

        while let Some(event) = central_events.next().await {
            for mapped in Self::map_central_event(&adapter, &devices, event).await {
                if let Err(err) = tx.try_send(mapped) {
                    if err.is_disconnected() {
                        return;
                    }
                    debug!("Device event channel full; dropping event");
                }
            }
        }
        debug!("btleplug central event stream ended");
    }

    /// Map a btleplug [`CentralEvent`] to zero or more [`DeviceEvent`]s,
    /// keeping the device cache current along the way.
    async fn map_central_event(
        adapter: &btleplug::platform::Adapter,
        devices: &DashMap<DeviceId, DiscoveredDevice>,
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

            CentralEvent::DeviceConnected(id) => Self::update_connected(devices, id, true),
            CentralEvent::DeviceDisconnected(id) => Self::update_connected(devices, id, false),

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
                match state {
                    CentralState::PoweredOn => {
                        info!("Bluetooth adapter powered on");
                    }
                    CentralState::PoweredOff => {
                        warn!(
                            "Bluetooth adapter powered off; device events will pause \
                             until it powers back on"
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

    /// Update the cached connection state of a device, emitting a
    /// [`DeviceEvent::DeviceUpdated`] when it changes.
    fn update_connected(
        devices: &DashMap<DeviceId, DiscoveredDevice>,
        id: btleplug::platform::PeripheralId,
        connected: bool,
    ) -> Vec<DeviceEvent> {
        let device_id = Self::peripheral_id_to_device_id(&id);
        let mut emitted = Vec::new();
        if let Some(mut entry) = devices.get_mut(&device_id) {
            if entry.value_mut().device.is_connected != connected {
                entry.value_mut().device.is_connected = connected;
                let device = entry.value().device.clone();
                emitted.push(DeviceEvent::DeviceUpdated {
                    device,
                    changed_fields: vec![UpdateField::Connected],
                });
            }
        }
        emitted
    }

    /// Look up a peripheral (cache first, then the adapter), convert it to a
    /// [`BluetoothDevice`], and refresh the cache entry.
    async fn fetch_and_cache_device(
        adapter: &btleplug::platform::Adapter,
        devices: &DashMap<DeviceId, DiscoveredDevice>,
        id: &btleplug::platform::PeripheralId,
    ) -> Option<BluetoothDevice> {
        let device_id = Self::peripheral_id_to_device_id(id);
        let peripheral = match devices.get(&device_id) {
            Some(entry) => {
                let peripheral = entry.value().peripheral.clone();
                drop(entry);
                peripheral
            }
            None => match adapter.peripheral(id).await {
                Ok(peripheral) => peripheral,
                Err(e) => {
                    debug!("Failed to get peripheral {}: {}", device_id, e);
                    return None;
                }
            },
        };

        let device = match Self::peripheral_to_device(&peripheral).await {
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
            .map_err(|e| match e {
                btleplug::Error::TimedOut(_) => Error::ConnectionTimeout,
                e => Error::BackendError {
                    backend: BackendKind::Btleplug,
                    message: format!("Connection failed: {}", e),
                },
            })?;

        info!("Connected to device: {}", device_id);
        if let Some(mut entry) = self.devices.get_mut(&device_id) {
            entry.value_mut().device.is_connected = true;
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
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Disconnection failed: {}", e),
            })?;

        if let Some(mut entry) = self.devices.get_mut(&device_id) {
            entry.value_mut().device.is_connected = false;
        }

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
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to check connection: {}", e),
            })
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
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Service discovery failed: {}", e),
            })?;

        // Get services from peripheral
        let services = peripheral.services();

        // Convert to our GattService type
        let gatt_services: Vec<GattService> = services
            .iter()
            .map(Self::btleplug_service_to_gatt)
            .collect();

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
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Read failed: {}", e),
            })?;

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
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Write failed: {}", e),
            })?;

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
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Subscription failed: {}", e),
            })?;

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
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Unsubscribe failed: {}", e),
            })?;

        debug!("Successfully unsubscribed from characteristic {}", uuid);
        Ok(())
    }

    async fn notifications(&self, id: &DeviceId) -> Result<NotificationStream> {
        // Get the peripheral and set up notification stream
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        drop(entry);

        // Start a task to read notifications
        tokio::spawn(async move {
            if let Ok(mut stream) = peripheral.notifications().await {
                while let Some(notification) = stream.next().await {
                    debug!("Notification received: {:?}", notification);
                    // In a real implementation, we'd forward this to a channel
                }
            }
        });

        // Return empty stream for now - a full implementation would return the actual stream
        Ok(Box::pin(stream::empty::<ValueNotification>()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
