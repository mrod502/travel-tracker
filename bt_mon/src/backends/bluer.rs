//! bluer backend implementation for bt_mon — Linux/BlueZ through D-Bus.
//!
//! # What one monitor owns
//!
//! One `bluer::Session`, one `Adapter`, and one discovery stream per scan. BlueZ holds the
//! discovery session for us (`StartDiscovery`/`StopDiscovery` behind a session token), so
//! unlike the btleplug backend there is no cache to poll: the discovery stream *is* the
//! event source, and re-opening it is both the re-arm (a session that went stale without
//! saying so) and the recovery after `bluetoothd` restarts.
//!
//! Per-device notifications are the exception: BlueZ gives one notification session per
//! *characteristic* (`Characteristic1.StartNotify`), so this backend keeps one pump per
//! `(device, characteristic)` and fans the frames into the same per-device stream the other
//! backends hand out.
//!
//! # Reading this file without a radio
//!
//! Everything here needs BlueZ, and this container has none, so the parts that are
//! *decisions* rather than D-Bus calls are split out as pure functions over plain data:
//! `device_snapshot` (what a set of BlueZ properties means), `reconstruct_advertisement`
//! (the payload re-frame and its two documented limits), `map_gatt_error` (which failure
//! is which), and `connection_report` (what a link event does to the cached device). The
//! platform call sits behind each of them, which is why the tests in this module run with
//! no adapter present.
//!
//! # What this backend cannot tell you
//!
//! * BlueZ exposes only the advertisement properties it parses, and `AdvertisingData` only
//!   the types it considers safe, so a payload from `reconstruct_advertisement` cannot
//!   support an argument from absence. That is why it is labelled
//!   [`RawPayloadSource::Reconstructed`](crate::RawPayloadSource).
//! * A device that is known but out of range still has a `Device` object: RSSI is `None`,
//!   and that is the only hint. `DeviceRemoved` means the object went away (roughly, the
//!   monitor stopped being told about it), not that it left the room.

use async_trait::async_trait;
use bluer::{Adapter, AdapterEvent, AdapterProperty, DiscoveryFilter, DiscoveryTransport, Uuid};
use dashmap::DashMap;
use futures::stream::StreamExt;
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex};
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
    ad_structure, BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId,
    GattCharacteristic, GattService, ServiceUuid, ValueNotification,
};

/// AD type for the `AdvertisingFlags` property (Bluetooth Assigned Numbers 0x01).
const AD_FLAGS: u8 = 0x01;
/// AD type for service data with a 16-bit UUID (0x16).
const AD_SERVICE_DATA_16: u8 = 0x16;
/// AD type for service data with a 32-bit UUID (0x20).
const AD_SERVICE_DATA_32: u8 = 0x20;
/// AD type for service data with a 128-bit UUID (0x21).
const AD_SERVICE_DATA_128: u8 = 0x21;
/// AD type for manufacturer-specific data (0xFF).
const AD_MANUFACTURER: u8 = 0xFF;

/// A device as BlueZ described it, in plain values, before any of this crate's decisions.
///
/// The seam [`device_snapshot`] is cut on: the platform reads happen in
/// [`BluerMonitor::read_advertisement`] and everything that follows is arithmetic on
/// values a test can write out by hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Advertisement {
    name: Option<String>,
    rssi: Option<i16>,
    is_connected: bool,
    services_resolved: bool,
    manufacturer_data: HashMap<u16, Vec<u8>>,
    service_data: HashMap<Uuid, Vec<u8>>,
    advertising_flags: Vec<u8>,
    advertising_data: HashMap<u8, Vec<u8>>,
}

/// Internal representation of a discovered device.
struct DiscoveredDevice {
    device: BluetoothDevice,
    /// The peer's own BlueZ object.
    ///
    /// This used to be an `address` filled from `adapter.address()`, which is *our* MAC,
    /// so every GATT call addressed the adapter's own object and the answers were about
    /// the wrong device (TD-03). Holding the handle rather than an address also removes a
    /// D-Bus round trip per operation: `Adapter::device()` is a constructor, not a query,
    /// and the eight call sites that each rebuilt it were rebuilding it wrong eight times.
    handle: bluer::Device,
}

/// bluer backend implementation of DeviceMonitor and GattClient.
pub struct BluerMonitor {
    adapter: Arc<Mutex<Adapter>>,
    adapter_name: String,
    devices: Arc<DashMap<DeviceId, DiscoveredDevice>>,
    /// Someone has asked for a scan. Distinct from whether BlueZ is delivering one; see
    /// [`DeviceMonitor::is_scanning`] and [`Self::health`].
    scanning: Arc<AtomicBool>,
    /// Set by `stop_scan` and `Drop` to end the discovery task promptly rather than at the
    /// next event or re-arm.
    shutdown: Arc<watch::Sender<bool>>,
    /// How often the discovery session is re-opened; see
    /// [`MonitorConfig::scan_interval`].
    scan_interval: Duration,
    events: DeviceEventSinks,
    notifications: NotificationHub,
    /// One BlueZ notification session per `(device, characteristic)`, because that is the
    /// granularity BlueZ offers (TD-02).
    notification_pumps: Arc<DashMap<(DeviceId, CharacteristicUuid), JoinHandle<()>>>,
    health: Arc<HealthState>,
}

impl BluerMonitor {
    /// Create a new bluer monitor on the default adapter.
    ///
    /// # Errors
    ///
    /// Returns an error if the BlueZ session cannot be established
    /// or if no Bluetooth adapter is found.
    pub async fn new() -> Result<Self> {
        Self::with_config(MonitorConfig::default()).await
    }

    /// Create a bluer monitor on the adapter and scan cadence `config` asks for.
    ///
    /// # Errors
    ///
    /// Returns an error if the BlueZ session cannot be established, if no
    /// Bluetooth adapter is found, or — when [`MonitorConfig::adapter`] is set —
    /// if the selector matches no adapter or matches more than one.
    pub async fn with_config(config: MonitorConfig) -> Result<Self> {
        debug!("Initializing bluer monitor");

        let session = bluer::Session::new()
            .await
            .map_err(|e| Error::InitFailed(format!("Failed to create bluer session: {}", e)))?;

        let adapter = match config.adapter.as_deref() {
            None => session
                .default_adapter()
                .await
                .map_err(|e| Error::InitFailed(format!("Failed to get default adapter: {}", e)))?,
            Some(selector) => Self::select_adapter(&session, selector).await?,
        };

        let adapter_name = adapter.name().to_string();
        debug!("Using adapter: {}", adapter_name);

        let is_powered = adapter.is_powered().await.map_err(|e| {
            Error::InitFailed(format!("Failed to check adapter power state: {}", e))
        })?;

        if !is_powered {
            warn!("Bluetooth adapter '{}' is not powered on.", adapter_name);
        }

        let health = Arc::new(if is_powered {
            HealthState::new()
        } else {
            HealthState::unpowered()
        });

        debug!("bluer monitor initialized successfully");

        Ok(Self {
            adapter: Arc::new(Mutex::new(adapter)),
            adapter_name,
            devices: Arc::new(DashMap::new()),
            scanning: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(watch::channel(true).0),
            scan_interval: config.scan_interval,
            events: DeviceEventSinks::new(),
            notifications: NotificationHub::new(),
            notification_pumps: Arc::new(DashMap::new()),
            health,
        })
    }

    /// Device events this monitor dropped because a consumer's channel was full.
    ///
    /// The streams are lossy by design; without a counter a consumer that is too slow to
    /// keep up is indistinguishable from a room with nothing in it.
    pub fn dropped_events(&self) -> u64 {
        self.events.dropped_events()
    }

    /// Pick the one adapter the operator named, or say what the session has.
    ///
    /// A BlueZ adapter is named by its D-Bus object (`hci0`), so the selector is
    /// matched against the name and, since an operator more often has the MAC
    /// address to hand, against the address too.
    async fn select_adapter(session: &bluer::Session, selector: &str) -> Result<Adapter> {
        let names = session
            .adapter_names()
            .await
            .map_err(|e| Error::InitFailed(format!("Failed to list adapters: {}", e)))?;

        let mut found = Vec::new();
        let mut matching = Vec::new();

        for name in &names {
            let address = match session.adapter(name) {
                Ok(adapter) => adapter
                    .address()
                    .await
                    .map(|address| address.to_string())
                    .unwrap_or_default(),
                Err(_) => String::new(),
            };
            let description = if address.is_empty() {
                name.clone()
            } else {
                format!("{name} ({address})")
            };
            found.push(description.clone());
            if adapter_matches(&description, selector) {
                match session.adapter(name) {
                    Ok(adapter) => matching.push(adapter),
                    Err(e) => {
                        return Err(Error::InitFailed(format!(
                            "Failed to open adapter '{name}': {e}"
                        )))
                    }
                }
            }
        }

        match matching.len() {
            1 => Ok(matching.pop().expect("one match counted")),
            0 if found.is_empty() => {
                Err(Error::InitFailed("No Bluetooth adapters found".to_string()))
            }
            0 => Err(Error::InitFailed(format!(
                "no Bluetooth adapter matches '{selector}'; this system has: {}",
                found.join(", ")
            ))),
            count => Err(Error::InitFailed(format!(
                "'{selector}' matches {count} adapters ({}); select one of them exactly",
                found.join(", ")
            ))),
        }
    }

    /// The id for a peer's address.
    ///
    /// BlueZ spells a device by its bare MAC, so there is nothing to strip: unlike the
    /// btleplug `PeripheralId` (`hci0/dev_...`), the adapter cannot leak into it (GAP_ANALYSIS
    /// M37). `DeviceId::new` is right for an address that is already canonical.
    fn address_to_device_id(address: &bluer::Address) -> DeviceId {
        DeviceId::new(address.to_string())
    }

    /// Read what BlueZ currently says about a peer, as plain values.
    ///
    /// Every property is optional in BlueZ and reading one that is absent is an error
    /// rather than an empty answer, so each is taken on its own merits: a device that has
    /// no `ManufacturerData` property is a device whose advertisement had none (or whose
    /// properties have not arrived yet — they are filled asynchronously after
    /// `DeviceAdded`), and neither case is a reason to lose the device. That is TD-04: this
    /// used to return two empty maps unconditionally, so a beacon advertising manufacturer
    /// data looked identical to one advertising nothing.
    async fn read_advertisement(handle: &bluer::Device) -> Advertisement {
        // Every optional BlueZ property answers `Ok(None)` when it is simply not there, so
        // each is flattened rather than unwrapped: an absent property is a fact about the
        // advertisement, not a reason to lose the device.
        Advertisement {
            name: handle.name().await.ok().flatten(),
            rssi: handle.rssi().await.ok().flatten(),
            is_connected: handle.is_connected().await.unwrap_or(false),
            services_resolved: handle.is_services_resolved().await.unwrap_or(false),
            manufacturer_data: handle
                .manufacturer_data()
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
            service_data: handle
                .service_data()
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
            advertising_flags: handle
                .advertising_flags()
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
            advertising_data: handle
                .advertising_data()
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
        }
    }

    /// The snapshot a peer's properties describe.
    async fn snapshot(handle: &bluer::Device) -> BluetoothDevice {
        let address = handle.address();
        device_snapshot(
            Self::address_to_device_id(&address),
            address.to_string(),
            &Self::read_advertisement(handle).await,
        )
    }

    /// The peer's BlueZ object for a device this monitor has discovered.
    fn handle(&self, id: &DeviceId) -> Result<bluer::Device> {
        self.devices
            .get(id)
            .map(|entry| entry.value().handle.clone())
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))
    }

    fn bluer_flags_to_characteristic_props(
        flags: bluer::gatt::CharacteristicFlags,
    ) -> CharacteristicProperties {
        CharacteristicProperties {
            broadcast: flags.broadcast,
            read: flags.read,
            write_without_response: flags.write_without_response,
            write: flags.write,
            notify: flags.notify,
            indicate: flags.indicate,
            authenticated_signed_write: flags.authenticated_signed_writes,
            extended_properties: flags.extended_properties,
        }
    }

    async fn bluer_service_to_gatt(service: &bluer::gatt::remote::Service) -> Result<GattService> {
        let uuid = service.uuid().await.map_err(|e| Error::BackendError {
            backend: BackendKind::Bluer,
            message: format!("Failed to get service UUID: {}", e),
        })?;
        let svc_uuid = ServiceUuid(uuid);

        let characteristics = service
            .characteristics()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to get characteristics: {}", e),
            })?;

        let mut gatt_characteristics = Vec::new();
        for char in characteristics.iter() {
            let char_uuid = char.uuid().await.map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to get characteristic UUID: {}", e),
            })?;
            let char_uuid = CharacteristicUuid(char_uuid);

            let flags = char.flags().await.map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to get characteristic flags: {}", e),
            })?;
            let props = Self::bluer_flags_to_characteristic_props(flags);

            gatt_characteristics.push(GattCharacteristic {
                uuid: char_uuid,
                properties: props,
                handle: None,
            });
        }

        Ok(GattService {
            uuid: svc_uuid,
            // BlueZ answers this; the `false` here used to be a placeholder for an API
            // that was there all along.
            is_primary: service.primary().await.unwrap_or(false),
            characteristics: gatt_characteristics,
        })
    }

    /// Find a characteristic by uuid, with the service it lives under.
    ///
    /// The service travels with it because [`ValueNotification::service`] needs it and
    /// BlueZ's notification frame carries only the characteristic's own object path.
    async fn find_characteristic(
        device: &bluer::Device,
        uuid: &CharacteristicUuid,
    ) -> Option<(ServiceUuid, bluer::gatt::remote::Characteristic)> {
        let services = device.services().await.ok()?;
        for service in services.iter() {
            let service_uuid = match service.uuid().await {
                Ok(uuid) => ServiceUuid(uuid),
                Err(_) => continue,
            };
            let Ok(characteristics) = service.characteristics().await else {
                continue;
            };
            for characteristic in characteristics.iter() {
                if let Ok(char_uuid) = characteristic.uuid().await {
                    if char_uuid == uuid.0 {
                        return Some((service_uuid, characteristic.clone()));
                    }
                }
            }
        }
        None
    }

    /// Starts the notification session for one characteristic and pumps its frames.
    ///
    /// BlueZ's `StartNotify` is per characteristic and its stream is single-consumer, so
    /// the session is opened once here and the frames are fanned out to every
    /// [`notifications`](GattClient::notifications) stream on the device — which is also
    /// why an unsubscribed characteristic must stop its own session rather than have its
    /// frames filtered out: the CCCD stays written for as long as the session lives.
    fn start_notification_pump(
        &self,
        device_id: &DeviceId,
        uuid: &CharacteristicUuid,
        service: ServiceUuid,
        characteristic: bluer::gatt::remote::Characteristic,
    ) {
        let key = (device_id.clone(), *uuid);
        self.stop_notification_pump(&key);

        let hub = self.notifications.clone();
        let pumps = self.notification_pumps.clone();
        let health = self.health.clone();
        let id = device_id.clone();
        let characteristic_id = *uuid;
        // The task removes its own entry when it ends, so it needs its own copy of the key
        // and of the map; the caller keeps the originals to insert the handle with.
        let task_key = key.clone();

        let handle = tokio::spawn(async move {
            let mut frames = match characteristic.notify().await {
                // Boxed because BlueZ's notification stream is `!Unpin` and this task has
                // to poll it in a `select!`.
                Ok(frames) => Box::pin(frames),
                Err(e) => {
                    // The CCCD write failed: say so where a consumer can find it, because
                    // the stream they already hold will simply never deliver.
                    warn!(
                        "Cannot start the notification session for {} on {}: {}; the \
                         streams opened on the device end rather than hang",
                        characteristic_id, id, e
                    );
                    health.note_error(format!(
                        "notification session for {characteristic_id} on {id} could not start: {e}"
                    ));
                    pumps.remove(&task_key);
                    hub.forget(&id);
                    return;
                }
            };

            debug!(
                "Notification session open for {} on {}",
                characteristic_id, id
            );
            let mut delivered = 0u64;
            let mut no_consumers = false;

            loop {
                tokio::select! {
                    biased;
                    // Dropping out here drops the stream, which drops the session token,
                    // which is what writes the CCCD back to off: nobody keeps the peer
                    // sending at a node that stopped listening.
                    _ = hub.wait_until_empty(&id) => {
                        no_consumers = true;
                        break;
                    }
                    frame = frames.next() => {
                        let Some(value) = frame else { break };
                        delivered += 1;
                        hub.publish(&id, ValueNotification {
                            characteristic: characteristic_id,
                            service: Some(service),
                            value,
                            timestamp: Some(std::time::Instant::now()),
                        });
                    }
                }
            }

            debug!(
                "Notification session for {} on {} ended after {} frame(s)",
                characteristic_id, id, delivered
            );
            if !no_consumers {
                health.note_error(format!(
                    "notification session for {characteristic_id} on {id} ended after \
                     {delivered} frame(s)"
                ));
            }
            pumps.remove(&task_key);
            hub.forget(&id);
        });

        // The task may already have run to completion (a session BlueZ refused, or a
        // consumer that went away before this point), in which case its own removal
        // happened before this insert and the entry would be left behind as a finished
        // handle. Check for that rather than leaving it: every later subscribe for this
        // characteristic would otherwise see a handle that can never deliver.
        let finished_already = handle.is_finished();
        if let Some(previous) = self.notification_pumps.insert(key.clone(), handle) {
            // Only reachable if two callers subscribed to the same characteristic at the
            // same moment; one session is enough and two would double the frames.
            previous.abort();
        }
        if finished_already {
            self.notification_pumps.remove(&key);
        }
    }

    /// Ends one characteristic's notification session.
    fn stop_notification_pump(&self, key: &(DeviceId, CharacteristicUuid)) {
        if let Some((_, handle)) = self.notification_pumps.remove(key) {
            handle.abort();
        }
    }

    /// Ends every session a device had, and the streams opened on it.
    fn forget_device_notifications(&self, device_id: &DeviceId) {
        let keys: Vec<(DeviceId, CharacteristicUuid)> = self
            .notification_pumps
            .iter()
            .filter(|entry| entry.key().0 == *device_id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in keys {
            self.stop_notification_pump(&key);
        }
        self.notifications.forget(device_id);
    }

    /// Publishes a change this node caused itself (a link it brought up or down).
    fn publish(&self, previous: &BluetoothDevice, reported: BluetoothDevice) {
        let event = report_event(previous, reported);
        self.events.publish(&event);
    }
}

/// The snapshot a peer's BlueZ properties describe.
///
/// Split out because every line of it is a decision. `manufacturer_data` and
/// `service_data` come from the properties BlueZ parsed *out* of the advertisement;
/// `raw_payload` is the same advertisement re-framed as structures (see
/// [`reconstruct_advertisement`]) and labelled `Reconstructed`, because a consumer that
/// finds the payload later cannot tell it apart from bytes the radio handed over unless the
/// record says so.
///
/// The name is deliberately *not* in the payload: BlueZ's `Name` may have come from GATT
/// resolution rather than the advertisement, and a structure invented from it would be an
/// advertisement that never happened.
fn device_snapshot(
    id: DeviceId,
    address: String,
    advertisement: &Advertisement,
) -> BluetoothDevice {
    let service_data = advertisement
        .service_data
        .iter()
        .map(|(uuid, data)| (ServiceUuid(*uuid), data.clone()))
        .collect();

    let device = BluetoothDevice {
        id,
        address,
        name: advertisement.name.clone(),
        rssi: advertisement.rssi.map(|rssi| rssi as i32),
        is_connected: advertisement.is_connected,
        manufacturer_data: advertisement.manufacturer_data.clone(),
        service_data,
        services_resolved: advertisement.services_resolved,
        raw_payload: None,
        raw_payload_source: None,
    };

    match reconstruct_advertisement(advertisement) {
        Some(payload) => device.with_reconstructed_raw_payload(payload),
        // Nothing reported at all: `None` means "BlueZ told us nothing about any
        // advertisement", which is what the field's contract already reserves it for.
        None => device,
    }
}

/// The Bluetooth Base UUID's tail (Core Vol 2 Part B §2.5.1).
///
/// A UUID whose last twelve bytes are these carries a 16- or 32-bit alias in its first
/// four, and BlueZ reports every service-data key as a full 128-bit UUID — so this tail is
/// the only way to tell which width the UUID went on air as, and the width decides the AD
/// type. Guessing 128 always would put a 4-byte structure on air as a 17-byte one and make
/// the same beacon look like two different devices to anything that parses the payload.
const BASE_UUID_TAIL: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5f, 0x9b, 0x34, 0xfb,
];

/// The AD type and on-air bytes for a service-data UUID, little-endian as they go out.
fn service_ad_uuid(uuid: &Uuid) -> (u8, Vec<u8>) {
    let bytes = uuid.as_bytes();

    if bytes[4..] != BASE_UUID_TAIL {
        return (AD_SERVICE_DATA_128, bytes.iter().rev().copied().collect());
    }
    if bytes[0] == 0 && bytes[1] == 0 {
        // A 16-bit alias sits in bytes 2..4, big-endian because UUIDs are, and comes out
        // the other way round because the air is little-endian.
        return (AD_SERVICE_DATA_16, vec![bytes[3], bytes[2]]);
    }
    (
        AD_SERVICE_DATA_32,
        bytes[..4].iter().rev().copied().collect(),
    )
}

/// The advertisement re-framed as Length-Type-Data structures.
///
/// BlueZ parses the on-air buffer into typed properties, so this re-assembles structures
/// rather than recovering bytes. Two consequences, both of which the payload's
/// [`RawPayloadSource`](crate::RawPayloadSource) label exists to carry:
///
/// * The order is by AD type, not the order on air, because the order is not reported.
///   Two payloads are comparable as "this device's advertisement changed", not as "the same
///   capture".
/// * An AD type that is missing here may be missing because BlueZ does not expose it
///   (`AdvertisingData` carries only the types it considers safe to hand to an
///   application). Absence is unknown, never evidence that the device omitted it.
///
/// A property that would duplicate a structure already present in `AdvertisingData` is
/// skipped, so manufacturer and service data are not written twice on a BlueZ version that
/// reports them in both places.
///
/// Returns `None` when there is nothing to say, so an empty payload cannot be read as "the
/// device advertised an empty payload".
fn reconstruct_advertisement(advertisement: &Advertisement) -> Option<Vec<u8>> {
    // (AD type, data) pairs; sorted before encoding so the payload is stable across
    // reports of the same advertisement, which is what makes comparing two of them
    // meaningful.
    let mut structures: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut reported_types: std::collections::HashSet<u8> = std::collections::HashSet::new();

    if !advertisement.advertising_flags.is_empty() {
        structures.push((AD_FLAGS, advertisement.advertising_flags.clone()));
        reported_types.insert(AD_FLAGS);
    }

    for (kind, data) in &advertisement.advertising_data {
        structures.push((*kind, data.clone()));
        reported_types.insert(*kind);
    }

    if !reported_types.contains(&AD_MANUFACTURER) && !advertisement.manufacturer_data.is_empty() {
        // One structure per company, as on air: the company id is little-endian and is
        // part of the data, not of the type.
        let mut companies: Vec<&u16> = advertisement.manufacturer_data.keys().collect();
        companies.sort_unstable();
        for company in companies {
            let mut data = company.to_le_bytes().to_vec();
            data.extend_from_slice(&advertisement.manufacturer_data[company]);
            structures.push((AD_MANUFACTURER, data));
        }
        reported_types.insert(AD_MANUFACTURER);
    }

    for (uuid, data) in &advertisement.service_data {
        let (kind, uuid_bytes) = service_ad_uuid(uuid);
        if reported_types.contains(&kind) {
            continue;
        }
        let mut payload = uuid_bytes;
        payload.extend_from_slice(data);
        structures.push((kind, payload));
    }

    structures.sort_by(|(a_kind, a_data), (b_kind, b_data)| {
        a_kind.cmp(b_kind).then_with(|| a_data.cmp(b_data))
    });

    let mut payload = Vec::new();
    for (kind, data) in structures {
        // A structure that cannot fit is left out rather than truncated: half a copied AD
        // type is a lie about what the device advertised.
        if let Some(structure) = ad_structure(kind, &data) {
            payload.extend_from_slice(&structure);
        }
    }

    if payload.is_empty() {
        None
    } else {
        Some(payload)
    }
}

/// Map a BlueZ failure from one GATT operation onto the variant a caller can act on.
///
/// BlueZ reports a *kind*, and the kinds are specific enough to be worth keeping:
/// `ServicesUnresolved` means the caller has to run `discover_services` first, which is not
/// the same advice as `DoesNotExist` (the peer's object is gone, so the link ended), and
/// neither is the advice for `NotPermitted` (the characteristic cannot serve this
/// operation). Everything else keeps the device, the call and BlueZ's own words, which is
/// what `GattError` carries and `BackendError` used to throw away (TD-12).
fn map_gatt_error(
    device_id: &DeviceId,
    operation: &str,
    uuid: Option<&CharacteristicUuid>,
    err: bluer::Error,
) -> Error {
    use bluer::ErrorKind as Kind;

    let context = match uuid {
        Some(uuid) => format!("{operation} of characteristic {uuid}"),
        None => operation.to_string(),
    };

    match err.kind {
        Kind::DoesNotExist | Kind::NotFound => Error::Disconnected(device_id.clone()),
        Kind::ServicesUnresolved => match uuid {
            Some(uuid) => Error::CharacteristicNotFound(*uuid),
            None => Error::InvalidArgument(format!(
                "{context}: services have not been resolved; call discover_services first"
            )),
        },
        Kind::NotPermitted => {
            Error::InvalidArgument(format!("{context} is not permitted: {}", err.message))
        }
        Kind::NotSupported => {
            Error::InvalidArgument(format!("{context} is not supported: {}", err.message))
        }
        Kind::InvalidArguments
        | Kind::InvalidLength
        | Kind::InvalidOffset
        | Kind::InvalidAddress(_)
        | Kind::InvalidName(_) => Error::InvalidArgument(format!("{context}: {}", err.message)),
        other => Error::GattError {
            device_id: device_id.clone(),
            operation: context,
            // Both halves matter: the kind is what BlueZ classified it as, the message is
            // what it said, and the second is the only part that names the failing
            // attribute on a stack this code has not been shown.
            message: format!("{other}: {}", err.message),
        },
    }
}

/// The device a link event describes, given what was cached before it.
///
/// Going down takes the resolved service table with it: the table belongs to the link, so
/// a device that reconnects has not had its services discovered until someone asks again.
fn connection_report(previous: &BluetoothDevice, connected: bool) -> BluetoothDevice {
    let mut reported = previous.clone();
    reported.is_connected = connected;
    if !connected {
        reported.services_resolved = false;
    }
    reported
}

/// Everything the discovery task needs, held by value so the task owns its inputs.
struct DiscoveryContext {
    /// A clone of the adapter, for building the `Device` objects the events name.
    ///
    /// Not behind the monitor's mutex: the previous version took it with `try_lock` and
    /// dropped the event outright when a GATT call happened to hold it, so a busy node
    /// quietly lost discoveries.
    adapter: Adapter,
    devices: Arc<DashMap<DeviceId, DiscoveredDevice>>,
    sinks: DeviceEventSinks,
    notifications: NotificationHub,
    notification_pumps: Arc<DashMap<(DeviceId, CharacteristicUuid), JoinHandle<()>>>,
    health: Arc<HealthState>,
    shutdown: watch::Receiver<bool>,
    scan_interval: Duration,
}

impl DiscoveryContext {
    /// Re-open the discovery session, counting the attempt either way.
    ///
    /// This is both the periodic re-arm and the recovery after BlueZ ended the session, and
    /// both are health signals: `rearm_attempts` climbing while `rearm_failures` climbs
    /// with it is a radio that is not coming back, and `discovering` goes false on the first
    /// failure so that a consumer can tell within one interval (TD-13).
    async fn rearm(&self) -> std::result::Result<DiscoveryStream, bluer::Error> {
        self.health.note_rearm_attempt();
        match self.adapter.discover_devices_with_changes().await {
            Ok(stream) => {
                self.health.note_rearm_success();
                // Every open after the first is a session BlueZ ended and this monitor went
                // back for: normal once per `bluetoothd` restart, alarming if it is climbing
                // every few seconds.
                self.health.note_event_stream_open();
                Ok(Box::pin(stream))
            }
            Err(e) => {
                self.health.note_rearm_failure(&e.to_string());
                Err(e)
            }
        }
    }
}

/// Record one discovery-stream event and publish what it means.
async fn handle_discovery_event(event: AdapterEvent, ctx: &DiscoveryContext) {
    match event {
        // `discover_devices_with_changes` re-sends this when a known device's properties
        // change, so "already in the cache" is not a reason to be quiet — it is the
        // difference between a first sighting and a report about a device we know.
        AdapterEvent::DeviceAdded(address) => {
            let id = BluerMonitor::address_to_device_id(&address);
            let Ok(handle) = ctx.adapter.device(address) else {
                debug!("BlueZ will not give us an object for {id}; nothing to report");
                return;
            };

            let device = BluerMonitor::snapshot(&handle).await;
            let previous = ctx.devices.insert(
                id.clone(),
                DiscoveredDevice {
                    device: device.clone(),
                    handle,
                },
            );

            let event = match previous {
                None => DeviceEvent::DeviceAdded { device },
                Some(previous) => report_event(&previous.device, device),
            };
            ctx.sinks.publish(&event);
        }

        AdapterEvent::DeviceRemoved(address) => {
            let id = BluerMonitor::address_to_device_id(&address);
            if ctx.devices.remove(&id).is_some() {
                // The link and every notification session on it went with the object.
                let keys: Vec<(DeviceId, CharacteristicUuid)> = ctx
                    .notification_pumps
                    .iter()
                    .filter(|entry| entry.key().0 == id)
                    .map(|entry| entry.key().clone())
                    .collect();
                for key in keys {
                    if let Some((_, handle)) = ctx.notification_pumps.remove(&key) {
                        handle.abort();
                    }
                }
                ctx.notifications.forget(&id);
                ctx.sinks.publish(&DeviceEvent::DeviceRemoved { id });
            }
        }

        // The radio's own state. `Discovering(false)` while a scan is still asked for is
        // exactly the TD-13 case: `is_scanning()` said true and nothing was being heard.
        AdapterEvent::PropertyChanged(AdapterProperty::Discovering(false)) => {
            ctx.health.set_radio_ready(false);
            ctx.health
                .note_error("BlueZ reported that discovery stopped on its own");
        }
        AdapterEvent::PropertyChanged(AdapterProperty::Powered(false)) => {
            ctx.health.set_adapter_powered(false);
            ctx.health
                .note_error("BlueZ reported that the adapter powered off");
        }
        AdapterEvent::PropertyChanged(AdapterProperty::Powered(true)) => {
            ctx.health.set_adapter_powered(true);
        }
        AdapterEvent::PropertyChanged(property) => {
            debug!("Adapter property changed: {property:?}");
        }
    }
}

/// Re-open the discovery session, which is both the periodic re-arm and the recovery
/// after BlueZ ended it.
///
/// Boxed because the stream `discover_devices_with_changes` returns is anonymous and has
/// to be replaced in place each time the session is re-armed.
type DiscoveryStream = std::pin::Pin<Box<dyn futures::stream::Stream<Item = AdapterEvent> + Send>>;

/// How long to wait before re-opening a discovery session that could not be re-armed.
///
/// Short enough that a `bluetoothd` restart costs a beat of reports, long enough that an
/// adapter which refuses discovery outright does not turn the task into a spin loop.
const REARM_RETRY: Duration = Duration::from_secs(2);

#[async_trait]
impl DeviceMonitor for BluerMonitor {
    async fn start_scan(&self) -> Result<()> {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Err(Error::ScanAlreadyInProgress);
        }

        debug!("Starting scan with bluer backend...");
        let adapter = self.adapter.lock().await;
        self.devices.clear();

        // BlueZ de-duplicates advertisements unless told otherwise, and the delta
        // threshold `StartDiscovery` applies by default is dropped once a filter is set —
        // so without this a beacon whose RSSI has not moved produces no event at all, and
        // `discover_devices_with_changes` has nothing to re-emit. The filter has to be set
        // before discovery starts, because the discovery session pushes it when it arms.
        let filter = DiscoveryFilter {
            duplicate_data: true,
            transport: DiscoveryTransport::Le,
            ..Default::default()
        };
        if let Err(e) = adapter.set_discovery_filter(filter).await {
            self.scanning.store(false, Ordering::SeqCst);
            self.health.set_radio_ready(false);
            self.health.note_error(e.to_string());
            return Err(Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to set the discovery filter: {e}"),
            });
        }

        let discovery: DiscoveryStream = match adapter.discover_devices_with_changes().await {
            Ok(discovery) => Box::pin(discovery),
            Err(e) => {
                self.scanning.store(false, Ordering::SeqCst);
                self.health.set_radio_ready(false);
                self.health.note_error(e.to_string());
                return Err(Error::BackendError {
                    backend: BackendKind::Bluer,
                    message: format!("Failed to start discovery: {e}"),
                });
            }
        };
        self.health.set_radio_ready(true);
        // The first of possibly many: every later open is a session BlueZ ended and the
        // discovery task went back for.
        self.health.note_event_stream_open();

        info!(
            "Scan started successfully on adapter '{}'",
            self.adapter_name
        );

        // A receiver made after this starts from "running". `send_replace`, not `send`:
        // `send` stores nothing when there is no receiver left, and the task from the
        // previous scan is exactly what may have gone, which would leave the stored value
        // at `true` and the next task breaking out the moment it starts.
        let _ = self.shutdown.send_replace(false);

        let ctx = DiscoveryContext {
            adapter: adapter.clone(),
            devices: self.devices.clone(),
            sinks: self.events.clone(),
            notifications: self.notifications.clone(),
            notification_pumps: self.notification_pumps.clone(),
            health: self.health.clone(),
            shutdown: self.shutdown.subscribe(),
            scan_interval: self.scan_interval,
        };
        drop(adapter);

        tokio::spawn(async move {
            let mut stream = discovery;
            let mut shutdown = ctx.shutdown.clone();

            loop {
                if *shutdown.borrow() {
                    break;
                }

                // `scan_interval` is how often the session is re-armed, so that a
                // discovery session BlueZ ended without telling us cannot leave a running
                // node deaf. `Duration::ZERO` turns re-arming off; a stream that ends on
                // its own is still reopened below, because that is recovery, not re-arming.
                let rearm_due = if ctx.scan_interval.is_zero() {
                    futures::future::Either::Left(std::future::pending::<()>())
                } else {
                    futures::future::Either::Right(time::sleep_until(
                        time::Instant::now() + ctx.scan_interval,
                    ))
                };

                tokio::select! {
                    biased;
                    // `stop_scan` or the monitor being dropped.
                    _ = shutdown.changed() => break,
                    event = stream.next() => match event {
                        Some(event) => handle_discovery_event(event, &ctx).await,
                        // BlueZ ended the session: the adapter went away, or `bluetoothd`
                        // restarted. Breaking here is what let a node run for hours
                        // reporting nothing while `is_scanning()` said true.
                        None => match ctx.rearm().await {
                            Ok(fresh) => stream = fresh,
                            Err(e) => {
                                warn!("discovery could not be re-opened: {e}; retrying");
                                time::sleep(REARM_RETRY).await;
                            }
                        },
                    },
                    _ = rearm_due => match ctx.rearm().await {
                        Ok(fresh) => {
                            debug!("discovery session re-armed after {:?}", ctx.scan_interval);
                            stream = fresh;
                        }
                        Err(e) => warn!("failed to re-arm discovery: {e}"),
                    },
                }
            }

            debug!("Discovery task for adapter '{}' ended", ctx.adapter.name());
        });

        time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    async fn stop_scan(&self) -> Result<()> {
        if !self.scanning.load(Ordering::SeqCst) {
            return Err(Error::NotScanning);
        }
        debug!("Stopping scan...");
        self.scanning.store(false, Ordering::SeqCst);
        // Ends the discovery task now rather than at its next event.
        let _ = self.shutdown.send_replace(true);
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
        let adapter = self.adapter.lock().await;
        adapter.is_powered().await.map_err(|e| Error::BackendError {
            backend: BackendKind::Bluer,
            message: format!("Failed to check power state: {}", e),
        })
    }

    async fn adapter_info(&self) -> Result<String> {
        let adapter = self.adapter.lock().await;
        let name = adapter.name();
        let address = adapter.address().await.map_err(|e| Error::BackendError {
            backend: BackendKind::Bluer,
            message: format!("Failed to get address: {}", e),
        })?;
        let powered = adapter
            .is_powered()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to check power: {}", e),
            })?;
        Ok(format!(
            "Adapter '{}' ({}): powered={}",
            name, address, powered
        ))
    }

    /// Device events, seeded with everything this monitor has already cached.
    ///
    /// Discovery runs whether or not anyone is listening (that is what `start_scan` means),
    /// so unlike the btleplug pump there is nothing to start here: this registers a
    /// consumer and replays the cache, so a stream opened an hour into a scan is not blind
    /// to every device in the room.
    async fn device_events(&self) -> Result<DeviceEventStream> {
        let replay = self
            .devices
            .iter()
            .map(|entry| DeviceEvent::DeviceAdded {
                device: entry.value().device.clone(),
            })
            .collect::<Vec<_>>();

        Ok(self.events.subscribe_seeded(replay))
    }

    async fn is_scanning(&self) -> Result<bool> {
        Ok(self.scanning.load(Ordering::SeqCst) && self.health.radio_ready())
    }

    /// The radio's own account: power, re-arm attempts and failures, and the last thing
    /// that went wrong (TD-13).
    fn health(&self) -> Option<MonitorHealth> {
        Some(self.health.snapshot(self.scanning.load(Ordering::SeqCst)))
    }
}

/// Ends discovery when the monitor is discarded.
///
/// The discovery task holds `Arc` clones of everything it needs, so dropping the monitor
/// is the only signal it has that nobody is listening.
impl Drop for BluerMonitor {
    fn drop(&mut self) {
        self.scanning.store(false, Ordering::SeqCst);
        let _ = self.shutdown.send_replace(true);
        for entry in self.notification_pumps.iter() {
            entry.value().abort();
        }
        self.notification_pumps.clear();
        self.notifications.forget_all();
    }
}

#[async_trait]
impl GattClient for BluerMonitor {
    async fn connect(&self, id: &DeviceId) -> Result<()> {
        debug!("Connecting to device: {}", id);
        let handle = self.handle(id)?;
        let device_id = id.clone();

        if let Err(e) = handle.connect().await {
            if e.kind != bluer::ErrorKind::AlreadyConnected {
                return Err(map_gatt_error(&device_id, "connect", None, e));
            }
            // The state that was asked for already holds. Failing here would send a caller
            // to retry something that has already happened, and BlueZ reports this for a
            // link it brought up on its own.
            debug!("Already connected to device: {}", device_id);
        }

        // A link this node brought up is reported like one the radio reported (TD-15): a
        // consumer driving off `device_events()` rather than polling now sees it.
        if let Some(mut entry) = self.devices.get_mut(&device_id) {
            let previous = entry.value().device.clone();
            entry.value_mut().device = connection_report(&previous, true);
            let reported = entry.value().device.clone();
            drop(entry);
            self.publish(&previous, reported);
        }

        info!("Connected to device: {}", device_id);
        Ok(())
    }

    async fn disconnect(&self, id: &DeviceId) -> Result<()> {
        debug!("Disconnecting from device: {}", id);
        let handle = self.handle(id)?;
        let device_id = id.clone();

        handle
            .disconnect()
            .await
            .map_err(|e| map_gatt_error(&device_id, "disconnect", None, e))?;

        if let Some(mut entry) = self.devices.get_mut(&device_id) {
            let previous = entry.value().device.clone();
            entry.value_mut().device = connection_report(&previous, false);
            let reported = entry.value().device.clone();
            drop(entry);
            self.publish(&previous, reported);
        }
        self.forget_device_notifications(&device_id);

        info!("Disconnected from device: {}", device_id);
        Ok(())
    }

    async fn is_connected(&self, id: &DeviceId) -> Result<bool> {
        let handle = self.handle(id)?;
        handle
            .is_connected()
            .await
            .map_err(|e| map_gatt_error(id, "is_connected", None, e))
    }

    async fn discover_services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        debug!("Discovering services for device: {}", id);
        let handle = self.handle(id)?;
        let device_id = id.clone();

        if !handle.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        let services = handle
            .services()
            .await
            .map_err(|e| map_gatt_error(&device_id, "discover_services", None, e))?;

        let mut gatt_services = Vec::new();
        for service in services.iter() {
            gatt_services.push(Self::bluer_service_to_gatt(service).await?);
        }

        // `ServicesResolved` is BlueZ's own property and it says true once the peer's
        // table has been walked, so this backend does not have to keep the answer by
        // itself the way btleplug does — but the transition still has to be published, or
        // a consumer waiting on the event stream for it never sees it (TD-08).
        if let Some(mut entry) = self.devices.get_mut(&device_id) {
            let previous = entry.value().device.clone();
            entry.value_mut().device.services_resolved = true;
            let reported = entry.value().device.clone();
            drop(entry);
            if previous.services_resolved != reported.services_resolved {
                self.publish(&previous, reported);
            }
        }

        info!(
            "Discovered {} services for device: {}",
            gatt_services.len(),
            device_id
        );
        Ok(gatt_services)
    }

    async fn services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        let handle = self.handle(id)?;
        let device_id = id.clone();

        let services = handle
            .services()
            .await
            .map_err(|e| map_gatt_error(&device_id, "services", None, e))?;

        let mut gatt_services = Vec::new();
        for service in services.iter() {
            gatt_services.push(Self::bluer_service_to_gatt(service).await?);
        }

        Ok(gatt_services)
    }

    async fn read_characteristic(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
    ) -> Result<Vec<u8>> {
        debug!("Reading characteristic {} from device {}", uuid, id);
        let handle = self.handle(id)?;
        let device_id = id.clone();

        if !handle.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        let (_service, characteristic) = Self::find_characteristic(&handle, uuid)
            .await
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        let value = characteristic
            .read()
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
        let handle = self.handle(id)?;
        let device_id = id.clone();

        if !handle.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        let (_service, characteristic) = Self::find_characteristic(&handle, uuid)
            .await
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        // BlueZ takes the write type as a request option; it used to be dropped, so a
        // caller that asked for a write-with-response and needed the acknowledgement got a
        // command and no error.
        let request = bluer::gatt::remote::CharacteristicWriteRequest {
            offset: 0,
            op_type: if response {
                bluer::gatt::WriteOp::Request
            } else {
                bluer::gatt::WriteOp::Command
            },
            prepare_authorize: false,
            ..Default::default()
        };

        characteristic
            .write_ext(value, &request)
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
        let handle = self.handle(id)?;
        let device_id = id.clone();

        if !handle.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }

        let Some((service, characteristic)) = Self::find_characteristic(&handle, uuid).await else {
            return Err(Error::CharacteristicNotFound(*uuid));
        };

        let flags = characteristic
            .flags()
            .await
            .map_err(|e| map_gatt_error(&device_id, "subscribe", Some(uuid), e))?;

        if !flags.notify && !flags.indicate {
            return Err(Error::InvalidArgument(format!(
                "characteristic {uuid} does not notify or indicate"
            )));
        }

        self.start_notification_pump(&device_id, uuid, service, characteristic);

        debug!("Successfully subscribed to characteristic {}", uuid);
        Ok(())
    }

    async fn unsubscribe(&self, id: &DeviceId, uuid: &CharacteristicUuid) -> Result<()> {
        debug!(
            "Unsubscribing from notifications for characteristic {} on device {}",
            uuid, id
        );
        // Checked for the error it used to be missing: this call used to do nothing at all
        // and still report success, so the CCCD stayed written and the peer kept sending.
        let _handle = self.handle(id)?;

        self.stop_notification_pump(&(id.clone(), *uuid));

        debug!("Successfully unsubscribed from characteristic {}", uuid);
        Ok(())
    }

    /// Every frame this device sends, fanned out to every caller.
    ///
    /// BlueZ gives one notification session per characteristic and hands each frame to a
    /// single reader, so this returns a view of the monitor's per-device fan-out (see
    /// [`NotificationHub`]) rather than a platform stream: two callers taking the same
    /// session would split the frames, each seeing an arbitrary subset with nothing to say
    /// it had.
    ///
    /// The sessions themselves are opened by [`subscribe`](GattClient::subscribe), which is
    /// where the CCCD gets written.
    async fn notifications(&self, id: &DeviceId) -> Result<NotificationStream> {
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
    use crate::types::RawPayloadSource;

    fn address() -> bluer::Address {
        bluer::Address::new([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
    }

    #[test]
    fn test_address_to_device_id() {
        let id = BluerMonitor::address_to_device_id(&address());
        assert_eq!(id.0, "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn test_bluer_uuid_conversion() {
        use uuid::Uuid;
        let test_uuid = Uuid::parse_str("0000110A-0000-1000-8000-00805F9B34FB").unwrap();
        let service_uuid = ServiceUuid(test_uuid);
        let char_uuid = CharacteristicUuid(test_uuid);
        assert_eq!(service_uuid.0, test_uuid);
        assert_eq!(char_uuid.0, test_uuid);
    }

    #[test]
    fn test_device_id_format() {
        let addr_public = bluer::Address::new([0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);
        let id = BluerMonitor::address_to_device_id(&addr_public);
        assert!(id.0.chars().all(|c| c.is_ascii_hexdigit() || c == ':'));
        assert_eq!(id.0.len(), 17);
    }

    #[test]
    #[cfg(feature = "bluer")]
    fn test_bluer_feature_compiles() {
        // Just verify bluer types are available when feature is enabled
        let _addr = bluer::Address::any();
        assert!(true);
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
    fn test_bluer_flags_to_characteristic_props() {
        // Test with flags set
        use bluer::gatt::CharacteristicFlags;
        let flags = CharacteristicFlags {
            broadcast: false,
            read: true,
            write_without_response: false,
            write: true,
            notify: true,
            indicate: false,
            authenticated_signed_writes: false,
            extended_properties: false,
            reliable_write: false,
            writable_auxiliaries: false,
            encrypt_read: false,
            encrypt_write: false,
            encrypt_authenticated_read: false,
            encrypt_authenticated_write: false,
            secure_read: false,
            secure_write: false,
            authorize: false,
        };
        let props = BluerMonitor::bluer_flags_to_characteristic_props(flags);
        assert!(props.notify);
        assert!(props.read);
        assert!(props.write);
        assert!(!props.indicate);
        assert!(!props.broadcast);

        // Test with default (all false)
        let default_flags = CharacteristicFlags::default();
        let props = BluerMonitor::bluer_flags_to_characteristic_props(default_flags);
        assert!(!props.notify);
        assert!(!props.read);
        assert!(!props.write);
    }

    #[test]
    fn manufacturer_and_service_data_reach_the_device() {
        // TD-04: these two were empty maps typed in by hand, so a beacon with a company
        // id in its advertisement was indistinguishable from one with nothing in it.
        let advertisement = Advertisement {
            name: Some("beacon-7".to_string()),
            rssi: Some(-63),
            manufacturer_data: HashMap::from([(0x004C, vec![0x02, 0x15])]),
            service_data: HashMap::from([(Uuid::from_u128(0x180f), vec![0x64])]),
            ..Default::default()
        };

        let device = device_snapshot(
            BluerMonitor::address_to_device_id(&address()),
            address().to_string(),
            &advertisement,
        );

        assert_eq!(device.name.as_deref(), Some("beacon-7"));
        assert_eq!(device.rssi, Some(-63));
        assert_eq!(
            device.manufacturer_data.get(&0x004C),
            Some(&vec![0x02, 0x15]),
            "the company id and its bytes, as BlueZ reported them"
        );
        assert_eq!(
            device
                .service_data
                .get(&ServiceUuid(Uuid::from_u128(0x180f))),
            Some(&vec![0x64])
        );
    }

    #[test]
    fn a_reconstructed_payload_is_labelled_as_one() {
        let advertisement = Advertisement {
            advertising_flags: vec![0x06],
            ..Default::default()
        };

        let device = device_snapshot(
            BluerMonitor::address_to_device_id(&address()),
            address().to_string(),
            &advertisement,
        );

        assert_eq!(
            device.raw_payload_source,
            Some(RawPayloadSource::Reconstructed),
            "a payload re-framed from BlueZ's properties has to say so, or a consumer \
             reading it back cannot tell an absent AD structure from one BlueZ never \
             exposed"
        );
        assert_eq!(
            device.raw_payload,
            Some(vec![0x02, AD_FLAGS, 0x06]),
            "one Length-Type-Data structure"
        );
    }

    #[test]
    fn a_payload_is_absent_when_bluez_reported_no_advertisement() {
        let device = device_snapshot(
            BluerMonitor::address_to_device_id(&address()),
            address().to_string(),
            &Advertisement::default(),
        );

        assert!(
            device.raw_payload.is_none() && device.raw_payload_source.is_none(),
            "nothing reported is not an empty advertisement, and an empty payload would \
             claim the device advertised an empty one"
        );
    }

    /// How BlueZ spells the 16-bit service UUID 0x180F: the Base UUID with the alias in
    /// its first four bytes.
    fn uuid_180f() -> Uuid {
        Uuid::parse_str("0000180f-0000-1000-8000-00805f9b34fb").unwrap()
    }

    #[test]
    fn a_reconstructed_payload_orders_structures_by_type() {
        // The on-air order is not reported, so the only order that can be compared across
        // two reports is one this function chooses.
        let advertisement = Advertisement {
            manufacturer_data: HashMap::from([(0x004C, vec![0x02, 0x15])]),
            advertising_flags: vec![0x06],
            service_data: HashMap::from([(uuid_180f(), vec![0x64])]),
            ..Default::default()
        };

        let first = reconstruct_advertisement(&advertisement).expect("a payload");
        let mut shuffled = advertisement.clone();
        // Same contents, built in a different order by a different report.
        shuffled.service_data = HashMap::new();
        shuffled.service_data.insert(uuid_180f(), vec![0x64]);
        shuffled.manufacturer_data = HashMap::new();
        shuffled.manufacturer_data.insert(0x004C, vec![0x02, 0x15]);
        let second = reconstruct_advertisement(&shuffled).expect("a payload");

        assert_eq!(first, second, "the payload has to be stable");
        assert!(
            first.starts_with(&[0x02, AD_FLAGS, 0x06]),
            "flags first: {first:02x?}"
        );
        assert!(
            !first.ends_with(&[0x02, AD_FLAGS, 0x06]),
            "and not the only one: {first:02x?}"
        );
        assert!(
            first.contains(&AD_MANUFACTURER),
            "manufacturer data keeps its AD type: {first:02x?}"
        );
        assert!(
            first.contains(&AD_SERVICE_DATA_16),
            "a 16-bit service uuid is re-framed as its own AD type: {first:02x?}"
        );
    }

    #[test]
    fn a_service_data_uuid_is_written_at_its_own_width_and_little_endian() {
        // BlueZ hands over a 128-bit UUID for every service-data key, so the width the
        // device actually used has to be recovered from the Base UUID or the structure comes
        // out with the wrong type and the wrong length.
        let advertisement = Advertisement {
            service_data: HashMap::from([
                // A 16-bit alias: 0000180f-0000-1000-8000-00805f9b34fb, which is what BlueZ
                // hands over for the 2-byte UUID 0x180F.
                (
                    Uuid::parse_str("0000180f-0000-1000-8000-00805f9b34fb").unwrap(),
                    vec![0x64],
                ),
                // A 32-bit alias: 12345678-0000-1000-8000-00805f9b34fb.
                (
                    Uuid::parse_str("12345678-0000-1000-8000-00805f9b34fb").unwrap(),
                    vec![0x01],
                ),
                // Not a Base UUID at all: 16 bytes on air, reversed.
                (
                    Uuid::parse_str("fda50693-a4e2-4fb1-cf6c-c6eb07647825").unwrap(),
                    vec![0x02],
                ),
            ]),
            ..Default::default()
        };

        let payload = reconstruct_advertisement(&advertisement).expect("a payload");
        assert_eq!(
            payload,
            vec![
                // 16-bit service data, sorted first by AD type.
                0x04,
                AD_SERVICE_DATA_16,
                0x0f,
                0x18,
                0x64,
                // 32-bit service data.
                0x06,
                AD_SERVICE_DATA_32,
                0x78,
                0x56,
                0x34,
                0x12,
                0x01,
                // 128-bit service data, the whole UUID reversed.
                0x12,
                AD_SERVICE_DATA_128,
                0x25,
                0x78,
                0x64,
                0x07,
                0xeb,
                0xc6,
                0x6c,
                0xcf,
                0xb1,
                0x4f,
                0xe2,
                0xa4,
                0x93,
                0x06,
                0xa5,
                0xfd,
                0x02,
            ],
            "{payload:02x?}"
        );
    }

    #[test]
    fn a_structure_already_reported_by_bluez_is_not_written_twice() {
        let advertisement = Advertisement {
            advertising_data: HashMap::from([(AD_MANUFACTURER, vec![0x4c, 0x00, 0x02, 0x15])]),
            manufacturer_data: HashMap::from([(0x004C, vec![0x02, 0x15])]),
            ..Default::default()
        };

        let payload = reconstruct_advertisement(&advertisement).expect("a payload");
        assert_eq!(payload, vec![0x05, AD_MANUFACTURER, 0x4c, 0x00, 0x02, 0x15]);
    }

    #[test]
    fn the_name_is_not_in_the_payload() {
        // BlueZ's `Name` may come from GATT resolution rather than the advertisement, so a
        // name structure built from it would be an advertisement that never happened.
        let advertisement = Advertisement {
            name: Some("beacon-7".to_string()),
            ..Default::default()
        };

        assert!(
            reconstruct_advertisement(&advertisement).is_none(),
            "a name alone is not an advertisement we can re-frame"
        );
    }

    #[test]
    fn a_link_going_down_takes_the_resolved_table_with_it() {
        let mut resolved = BluetoothDevice::new(
            BluerMonitor::address_to_device_id(&address()),
            address().to_string(),
        )
        .with_connected(true);
        resolved.services_resolved = true;

        let down = connection_report(&resolved, false);
        assert!(!down.is_connected);
        assert!(
            !down.services_resolved,
            "the service table belongs to the link"
        );

        let event = report_event(&resolved, down.clone());
        match event {
            DeviceEvent::DeviceUpdated { changed_fields, .. } => {
                assert_eq!(
                    changed_fields,
                    vec![UpdateField::ServicesResolved, UpdateField::Connected]
                );
            }
            other => panic!("a link going down is a change: {other:?}"),
        }
    }

    #[test]
    fn a_bluez_failure_becomes_the_variant_a_caller_can_act_on() {
        let id = BluerMonitor::address_to_device_id(&address());
        let uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
        let failing = |kind: bluer::ErrorKind, message: &str| bluer::Error {
            kind,
            message: message.to_string(),
        };

        assert!(matches!(
            map_gatt_error(&id, "read", Some(&uuid), failing(bluer::ErrorKind::ServicesUnresolved, "unresolved")),
            Error::CharacteristicNotFound(missing) if missing == uuid
        ));
        assert!(
            matches!(
                map_gatt_error(&id, "connect", None, failing(bluer::ErrorKind::DoesNotExist, "gone")),
                Error::Disconnected(gone) if gone == id
            ),
            "the peer's object going away is a link that ended"
        );
        assert!(matches!(
            map_gatt_error(
                &id,
                "read",
                Some(&uuid),
                failing(bluer::ErrorKind::NotPermitted, "not permitted")
            ),
            Error::InvalidArgument(_)
        ));
        assert!(matches!(
            map_gatt_error(
                &id,
                "write",
                Some(&uuid),
                failing(bluer::ErrorKind::InvalidArguments, "bad offset")
            ),
            Error::InvalidArgument(_)
        ));

        let error = map_gatt_error(
            &id,
            "write",
            Some(&uuid),
            failing(bluer::ErrorKind::Failed, "Write Failed"),
        );
        match error {
            Error::GattError {
                device_id,
                operation,
                message,
            } => {
                assert_eq!(device_id, id);
                assert_eq!(operation, format!("write of characteristic {uuid}"));
                assert!(
                    message.contains("Failed") && message.contains("Write Failed"),
                    "both BlueZ's classification and its own words: {message}"
                );
            }
            other => panic!("an unclassifiable BlueZ failure keeps the call: {other:?}"),
        }
    }
}
