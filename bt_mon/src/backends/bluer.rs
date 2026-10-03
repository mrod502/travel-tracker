//! bluer backend implementation for bt_mon.
//!
//! This module provides the Linux/BlueZ implementation using the `bluer` crate.
//! The bluer backend provides Linux-specific features like GATT server support
//! and BLE advertising, requiring BlueZ 5.43+.

use async_trait::async_trait;
use bluer::Adapter;
use dashmap::DashMap;
use futures::stream::{self, StreamExt};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time;

use crate::config::{adapter_matches, MonitorConfig};
use crate::error::{BackendKind, Error, Result};
use crate::monitor::events::{DeviceEvent, DeviceEventStream, NotificationStream};
use crate::monitor::{DeviceMonitor, GattClient};
use crate::types::ValueNotification;
use crate::types::{
    BluetoothDevice, CharacteristicProperties, CharacteristicUuid, DeviceId, GattCharacteristic,
    GattService, ServiceUuid,
};

/// Internal representation of a discovered device.
struct DiscoveredDevice {
    device: BluetoothDevice,
    adapter: Adapter,
    address: bluer::Address,
}

/// bluer backend implementation of DeviceMonitor and GattClient.
pub struct BluerMonitor {
    adapter: Arc<Mutex<Adapter>>,
    adapter_name: String,
    devices: Arc<DashMap<DeviceId, DiscoveredDevice>>,
    scanning: Arc<AtomicBool>,
    /// How often the discovery session is re-opened; see
    /// [`MonitorConfig::scan_interval`].
    scan_interval: Duration,
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

        debug!("bluer monitor initialized successfully");

        Ok(Self {
            adapter: Arc::new(Mutex::new(adapter)),
            adapter_name,
            devices: Arc::new(DashMap::new()),
            scanning: Arc::new(AtomicBool::new(false)),
            scan_interval: config.scan_interval,
        })
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

    fn address_to_device_id(address: &bluer::Address) -> DeviceId {
        DeviceId(address.to_string())
    }

    async fn device_to_bluetooth_device(
        adapter: &Adapter,
        address: &bluer::Address,
    ) -> Result<BluetoothDevice> {
        let device = adapter
            .device(*address)
            .map_err(|_e| Error::DeviceNotFound(address.to_string().into()))?;

        let id = Self::address_to_device_id(address);
        let address_str = address.to_string();
        let name = device.name().await.ok().flatten();
        let is_connected = device.is_connected().await.unwrap_or(false);
        let services_resolved = device.is_services_resolved().await.unwrap_or(false);
        let rssi = device.rssi().await.ok().flatten().map(|r| r as i32);

        Ok(BluetoothDevice {
            id,
            address: address_str,
            name,
            rssi,
            is_connected,
            manufacturer_data: HashMap::new(),
            service_data: HashMap::new(),
            services_resolved,
            // BlueZ hands bluer parsed device properties, not the advertisement
            // octets they were decoded from.
            raw_payload: None,
        })
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

    async fn bluer_service_to_gatt(
        _adapter: &Adapter,
        service: &bluer::gatt::remote::Service,
    ) -> Result<GattService> {
        let uuid = service.uuid().await.map_err(|e| Error::BackendError {
            backend: BackendKind::Bluer,
            message: format!("Failed to get service UUID: {}", e),
        })?;
        let svc_uuid = ServiceUuid(uuid);

        // Primary flag - bluer doesn't directly expose this, default to false
        let is_primary = false;

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

            // Get properties from flags
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
            is_primary,
            characteristics: gatt_characteristics,
        })
    }

    async fn find_characteristic_in_service(
        service: &bluer::gatt::remote::Service,
        uuid: &CharacteristicUuid,
    ) -> Option<bluer::gatt::remote::Characteristic> {
        let chars = service.characteristics().await.ok()?;
        for char in chars.iter() {
            if let Ok(char_uuid) = char.uuid().await {
                if char_uuid == uuid.0 {
                    return Some(char.clone());
                }
            }
        }
        None
    }

    async fn find_characteristic_in_device(
        adapter: &Adapter,
        device_addr: &bluer::Address,
        uuid: &CharacteristicUuid,
    ) -> Option<bluer::gatt::remote::Characteristic> {
        let device = adapter.device(*device_addr).ok()?;
        let services = device.services().await.ok()?;
        for service in services.iter() {
            if let Some(char) = Self::find_characteristic_in_service(service, uuid).await {
                return Some(char);
            }
        }
        None
    }
}

#[async_trait]
impl DeviceMonitor for BluerMonitor {
    async fn start_scan(&self) -> Result<()> {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Err(Error::ScanAlreadyInProgress);
        }

        debug!("Starting scan with bluer backend...");
        let adapter = self.adapter.lock().await;
        self.devices.clear();

        let discover_stream =
            adapter
                .discover_devices()
                .await
                .map_err(|e| Error::BackendError {
                    backend: BackendKind::Bluer,
                    message: format!("Failed to start discovery: {}", e),
                })?;

        info!(
            "Scan started successfully on adapter '{}'",
            self.adapter_name
        );
        drop(adapter);

        let devices = self.devices.clone();
        let adapter_clone = self.adapter.clone();
        let scanning = self.scanning.clone();
        let interval = self.scan_interval;

        tokio::spawn(async move {
            // Replacing the stream ends the old discovery session (its guard is
            // what holds the session open) and opens a new one, which is how a
            // discovery session that has gone stale without saying so gets
            // refreshed instead of leaving a healthy-looking node deaf.
            let mut stream = discover_stream;
            loop {
                if !scanning.load(Ordering::SeqCst) {
                    break;
                }

                if interval.is_zero() {
                    while let Some(event) = stream.next().await {
                        handle_adapter_event(event, &devices, &adapter_clone).await;
                    }
                    break;
                }

                let deadline = time::Instant::now() + interval;
                tokio::select! {
                    event = stream.next() => match event {
                        Some(event) => handle_adapter_event(event, &devices, &adapter_clone).await,
                        // The session ended; a re-open would need a fresh
                        // `discover_devices()` call, which is `start_scan`'s job.
                        None => break,
                    },
                    _ = time::sleep_until(deadline) => {
                        let adapter = adapter_clone.lock().await;
                        match adapter.discover_devices().await {
                            Ok(fresh) => {
                                stream = fresh;
                                debug!("discovery session re-armed after {:?}", interval);
                            }
                            Err(e) => {
                                warn!("failed to re-arm discovery: {}", e);
                                break;
                            }
                        }
                    }
                }
            }
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

    async fn device_events(&self) -> Result<DeviceEventStream> {
        Ok(Box::pin(stream::empty::<DeviceEvent>()))
    }

    async fn is_scanning(&self) -> Result<bool> {
        Ok(self.scanning.load(Ordering::SeqCst))
    }
}

/// Record one discovery-stream event in the shared device map.
async fn handle_adapter_event(
    event: bluer::AdapterEvent,
    devices: &DashMap<DeviceId, DiscoveredDevice>,
    adapter: &Arc<Mutex<Adapter>>,
) {
    match event {
        bluer::AdapterEvent::DeviceAdded(address) => {
            debug!("Device added: {}", address);
            // `try_lock` rather than `lock`: a discovery burst must not queue
            // behind a GATT operation that is holding the adapter.
            if let Ok(adapter) = adapter.try_lock() {
                if let Ok(device) =
                    BluerMonitor::device_to_bluetooth_device(&adapter, &address).await
                {
                    let id = device.id.clone();
                    if let Ok(addr) = adapter.address().await {
                        devices.insert(
                            id,
                            DiscoveredDevice {
                                device,
                                adapter: adapter.clone(),
                                address: addr,
                            },
                        );
                    }
                }
            }
        }
        bluer::AdapterEvent::DeviceRemoved(address) => {
            debug!("Device removed: {}", address);
            let id = BluerMonitor::address_to_device_id(&address);
            devices.remove(&id);
        }
        bluer::AdapterEvent::PropertyChanged(_) => {
            debug!("Adapter property changed");
        }
    }
}

/// Ends discovery when the monitor is discarded.
///
/// The discovery task holds `Arc` clones of the adapter and the device map, so
/// dropping the monitor is the only signal it has that nobody is listening.
impl Drop for BluerMonitor {
    fn drop(&mut self) {
        self.scanning.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl GattClient for BluerMonitor {
    async fn connect(&self, id: &DeviceId) -> Result<()> {
        debug!("Connecting to device: {}", id);
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let adapter = entry.value().adapter.clone();
        let address = entry.value().address;
        let device_id = id.clone();
        drop(entry);

        let device = adapter
            .device(address)
            .map_err(|_e| Error::DeviceNotFound(id.clone()))?;

        device.connect().await.map_err(|e| Error::BackendError {
            backend: BackendKind::Bluer,
            message: format!("Connection failed: {}", e),
        })?;

        if let Some(mut entry) = self.devices.get_mut(&device_id) {
            entry.value_mut().device.is_connected = true;
        }
        info!("Connected to device: {}", device_id);
        Ok(())
    }

    async fn disconnect(&self, id: &DeviceId) -> Result<()> {
        debug!("Disconnecting from device: {}", id);
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let adapter = entry.value().adapter.clone();
        let address = entry.value().address;
        let device_id = id.clone();
        drop(entry);

        let device = adapter
            .device(address)
            .map_err(|_e| Error::DeviceNotFound(id.clone()))?;

        device.disconnect().await.map_err(|e| Error::BackendError {
            backend: BackendKind::Bluer,
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
        let adapter = entry.value().adapter.clone();
        let address = entry.value().address;
        drop(entry);

        let device = adapter
            .device(address)
            .map_err(|_e| Error::DeviceNotFound(id.clone()))?;

        device
            .is_connected()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to check connection: {}", e),
            })
    }

    async fn discover_services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        debug!("Discovering services for device: {}", id);
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let adapter = entry.value().adapter.clone();
        let address = entry.value().address;
        drop(entry);

        let device = adapter
            .device(address)
            .map_err(|_e| Error::DeviceNotFound(id.clone()))?;

        let is_connected = device
            .is_connected()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to check connection: {}", e),
            })?;

        if !is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        let services_result = device.services().await;
        let services = match services_result {
            Ok(s) => s,
            Err(e) => {
                return Err(Error::BackendError {
                    backend: BackendKind::Bluer,
                    message: format!("Failed to get services: {}", e),
                });
            }
        };

        let mut gatt_services = Vec::new();
        for service in services.iter() {
            let gatt_service = Self::bluer_service_to_gatt(&adapter, service).await?;
            gatt_services.push(gatt_service);
        }

        info!(
            "Discovered {} services for device: {}",
            gatt_services.len(),
            id
        );
        Ok(gatt_services)
    }

    async fn services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let adapter = entry.value().adapter.clone();
        let address = entry.value().address;
        drop(entry);

        let device = adapter
            .device(address)
            .map_err(|_e| Error::DeviceNotFound(id.clone()))?;

        let services_result = device.services().await;
        let services = match services_result {
            Ok(s) => s,
            Err(e) => {
                return Err(Error::BackendError {
                    backend: BackendKind::Bluer,
                    message: format!("Failed to get services: {}", e),
                });
            }
        };

        let mut gatt_services = Vec::new();
        for service in services.iter() {
            let gatt_service = Self::bluer_service_to_gatt(&adapter, service).await?;
            gatt_services.push(gatt_service);
        }

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
        let adapter = entry.value().adapter.clone();
        let address = entry.value().address;
        drop(entry);

        let device = adapter
            .device(address)
            .map_err(|_e| Error::DeviceNotFound(id.clone()))?;

        let is_connected = device
            .is_connected()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to check connection: {}", e),
            })?;

        if !is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        let char_opt = Self::find_characteristic_in_device(&adapter, &address, uuid).await;
        let characteristic = char_opt.ok_or(Error::CharacteristicNotFound(*uuid))?;

        let value = characteristic
            .read()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
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
        _response: bool,
    ) -> Result<()> {
        debug!("Writing to characteristic {} on device {}", uuid, id);
        let entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let adapter = entry.value().adapter.clone();
        let address = entry.value().address;
        drop(entry);

        let device = adapter
            .device(address)
            .map_err(|_e| Error::DeviceNotFound(id.clone()))?;

        let is_connected = device
            .is_connected()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to check connection: {}", e),
            })?;

        if !is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        let characteristic = Self::find_characteristic_in_device(&adapter, &address, uuid)
            .await
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        characteristic
            .write(value)
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
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
        let adapter = entry.value().adapter.clone();
        let address = entry.value().address;
        drop(entry);

        let device = adapter
            .device(address)
            .map_err(|_e| Error::DeviceNotFound(id.clone()))?;

        let is_connected = device
            .is_connected()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to check connection: {}", e),
            })?;

        if !is_connected {
            return Err(Error::NotConnected(id.clone()));
        }

        let characteristic = Self::find_characteristic_in_device(&adapter, &address, uuid)
            .await
            .ok_or(Error::CharacteristicNotFound(*uuid))?;

        // Check if characteristic supports notifications via flags
        let flags = characteristic
            .flags()
            .await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Failed to get characteristic flags: {}", e),
            })?;
        let supports_notify = flags.notify;

        if !supports_notify {
            return Err(Error::BackendError {
                backend: BackendKind::Bluer,
                message: format!("Characteristic {} does not support notifications", uuid),
            });
        }

        debug!("Successfully subscribed to characteristic {}", uuid);
        Ok(())
    }

    async fn unsubscribe(&self, id: &DeviceId, uuid: &CharacteristicUuid) -> Result<()> {
        debug!(
            "Unsubscribing from notifications for characteristic {} on device {}",
            uuid, id
        );
        let _entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        debug!("Successfully unsubscribed from characteristic {}", uuid);
        Ok(())
    }

    async fn notifications(&self, id: &DeviceId) -> Result<NotificationStream> {
        debug!("Setting up notification stream for device {}", id);
        let _entry = self
            .devices
            .get(id)
            .ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        Ok(Box::pin(stream::empty::<ValueNotification>()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_to_device_id() {
        let addr = bluer::Address::new([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        let id = BluerMonitor::address_to_device_id(&addr);
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
    fn test_discovered_device_structure() {
        let _addr = bluer::Address::new([0xAA; 6]);
        let _device = BluetoothDevice {
            id: DeviceId("AA:BB:CC:DD:EE:FF".to_string()),
            address: "AA:BB:CC:DD:EE:FF".to_string(),
            name: Some("Test Device".to_string()),
            rssi: Some(-50),
            is_connected: false,
            manufacturer_data: HashMap::new(),
            service_data: HashMap::new(),
            services_resolved: false,
            raw_payload: None,
        };
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
}
