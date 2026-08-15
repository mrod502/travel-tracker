//! btleplug backend implementation for bt_mon.
//!
//! This module provides the cross-platform implementation using the `btleplug` crate.

use async_trait::async_trait;
use btleplug::api::{Central as _, Manager as _, Peripheral as _, ScanFilter, WriteType};
use dashmap::DashMap;
use futures::stream::{self, StreamExt};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time;

use crate::error::{BackendKind, Error, Result};
use crate::monitor::events::{DeviceEvent, DeviceEventStream, NotificationStream};
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
    scanning: Arc<Mutex<bool>>,
}

impl BtleplugMonitor {
    /// Create a new btleplug monitor.
    ///
    /// # Errors
    ///
    /// Returns an error if the btleplug manager cannot be initialized
    /// or if no Bluetooth adapter is found.
    pub async fn new() -> Result<Self> {
        debug!("Initializing btleplug monitor");
        
        // Create manager
        let manager = btleplug::platform::Manager::new().await
            .map_err(|e| Error::InitFailed(format!("Failed to create btleplug manager: {}", e)))?;
        
        // Get adapters using the Manager trait
        let adapters = manager.adapters().await
            .map_err(|e| Error::InitFailed(format!("Failed to get adapters: {}", e)))?;
        
        // Get the first available adapter
        let adapter = adapters.into_iter()
            .next()
            .ok_or_else(|| Error::InitFailed("No Bluetooth adapters found".to_string()))?;
        
        // Check if adapter is powered using Central trait
        let state = adapter.adapter_state().await
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
            scanning: Arc::new(Mutex::new(false)),
        })
    }

    /// Convert btleplug peripheral ID to our DeviceId format.
    fn peripheral_id_to_device_id(id: &btleplug::platform::PeripheralId) -> DeviceId {
        // btleplug peripheral IDs are platform-specific
        format!("{}", id).into()
    }

    /// Convert btleplug peripheral to our BluetoothDevice type.
    async fn peripheral_to_device(peripheral: &btleplug::platform::Peripheral) -> Result<BluetoothDevice> {
        let id = Self::peripheral_id_to_device_id(&peripheral.id());
        let address = peripheral.address().to_string();
        
        // Get properties from the peripheral
        let props = peripheral.properties().await
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
        })
    }

    /// Convert btleplug CharPropFlags to our CharacteristicProperties.
    fn flags_to_props(flags: btleplug::api::CharPropFlags) -> CharacteristicProperties {
        CharacteristicProperties {
            broadcast: flags.contains(btleplug::api::CharPropFlags::BROADCAST),
            read: flags.contains(btleplug::api::CharPropFlags::READ),
            write_without_response: flags.contains(btleplug::api::CharPropFlags::WRITE_WITHOUT_RESPONSE),
            write: flags.contains(btleplug::api::CharPropFlags::WRITE),
            notify: flags.contains(btleplug::api::CharPropFlags::NOTIFY),
            indicate: flags.contains(btleplug::api::CharPropFlags::INDICATE),
            authenticated_signed_write: flags.contains(btleplug::api::CharPropFlags::AUTHENTICATED_SIGNED_WRITES),
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
    fn find_characteristic(peripheral: &btleplug::platform::Peripheral, uuid: &CharacteristicUuid) -> Option<btleplug::api::Characteristic> {
        peripheral.characteristics()
            .into_iter()
            .find(|c| c.uuid == uuid.0)
    }
}

impl From<String> for DeviceId {
    fn from(s: String) -> Self {
        DeviceId(s)
    }
}

#[async_trait]
impl DeviceMonitor for BtleplugMonitor {
    async fn start_scan(&self) -> Result<()> {
        let mut scanning = self.scanning.lock().await;
        
        if *scanning {
            return Err(Error::ScanAlreadyInProgress);
        }
        
        debug!("Starting scan...");
        
        let adapter = self.adapter.lock().await;
        
        // Clear existing devices first
        self.devices.clear();
        
        // Start scanning
        adapter.start_scan(ScanFilter::default()).await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to start scan: {}", e),
            })?;
        
        *scanning = true;
        info!("Scan started successfully");
        
        drop(adapter);
        
        // Give some time for initial discoveries
        time::sleep(Duration::from_millis(500)).await;
        
        // Get discovered peripherals using Central trait
        let adapter = self.adapter.lock().await;
        let peripherals = adapter.peripherals().await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to get peripherals: {}", e),
            })?;
        
        for peripheral in peripherals {
            let device = Self::peripheral_to_device(&peripheral).await?;
            let id = device.id.clone();
            
            // Cache the device
            let peripheral_clone = peripheral.clone();
            self.devices.insert(id.clone(), DiscoveredDevice {
                device: device.clone(),
                peripheral: peripheral_clone,
            });
            
            info!("Discovered device: {} (name: {:?}, rssi: {:?})", 
                  id, device.name, device.rssi);
        }
        
        Ok(())
    }

    async fn stop_scan(&self) -> Result<()> {
        let mut scanning = self.scanning.lock().await;
        
        if !*scanning {
            return Err(Error::NotScanning);
        }
        
        debug!("Stopping scan...");
        
        let adapter = self.adapter.lock().await;
        adapter.stop_scan().await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to stop scan: {}", e),
            })?;
        
        *scanning = false;
        info!("Scan stopped successfully");
        
        Ok(())
    }

    async fn devices(&self) -> Result<Vec<BluetoothDevice>> {
        let devices: Vec<BluetoothDevice> = self.devices
            .iter()
            .map(|entry| entry.value().device.clone())
            .collect();
        
        debug!("Returning {} discovered devices", devices.len());
        Ok(devices)
    }

    async fn device(&self, id: &DeviceId) -> Result<BluetoothDevice> {
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        Ok(entry.value().device.clone())
    }

    async fn is_powered(&self) -> Result<bool> {
        let adapter = self.adapter.lock().await;
        let state = adapter.adapter_state().await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to check adapter state: {}", e),
            })?;
        Ok(state == btleplug::api::CentralState::PoweredOn)
    }

    async fn adapter_info(&self) -> Result<String> {
        let adapter = self.adapter.lock().await;
        adapter.adapter_info().await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to get adapter info: {}", e),
            })
    }

    async fn device_events(&self) -> Result<DeviceEventStream> {
        let stream = stream::empty::<DeviceEvent>();
        Ok(Box::pin(stream))
    }

    async fn is_scanning(&self) -> Result<bool> {
        let scanning = self.scanning.lock().await;
        Ok(*scanning)
    }
}

#[async_trait]
impl GattClient for BtleplugMonitor {
    async fn connect(&self, id: &DeviceId) -> Result<()> {
        debug!("Connecting to device: {}", id);
        
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();
        
        drop(entry);
        
        // Connect with timeout using the trait method
        peripheral.connect_with_timeout(Duration::from_secs(10)).await
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
        
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();
        
        drop(entry);
        
        peripheral.disconnect().await
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
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        entry.value().peripheral.is_connected().await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Failed to check connection: {}", e),
            })
    }

    async fn discover_services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        debug!("Discovering services for device: {}", id);
        
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        let device_id = id.clone();
        
        drop(entry);
        
        if !peripheral.is_connected().await.unwrap_or(false) {
            return Err(Error::NotConnected(device_id));
        }
        
        // Discover services
        peripheral.discover_services().await
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
        
        info!("Discovered {} services for device: {}", gatt_services.len(), device_id);
        Ok(gatt_services)
    }

    async fn services(&self, id: &DeviceId) -> Result<Vec<GattService>> {
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
        let peripheral = entry.value().peripheral.clone();
        drop(entry);
        
        let services = peripheral.services();
        let gatt_services: Vec<GattService> = services
            .iter()
            .map(Self::btleplug_service_to_gatt)
            .collect();
        
        Ok(gatt_services)
    }

    async fn read_characteristic(&self, id: &DeviceId, uuid: &CharacteristicUuid) -> Result<Vec<u8>> {
        debug!("Reading characteristic {} from device {}", uuid, id);
        
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
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
        let value = peripheral.read(&characteristic).await
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
        
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
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
        peripheral.write(&characteristic, value, write_type).await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Write failed: {}", e),
            })?;
        
        debug!("Successfully wrote {} bytes to characteristic {}", value.len(), uuid);
        Ok(())
    }

    async fn subscribe(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
    ) -> Result<()> {
        debug!("Subscribing to notifications for characteristic {} on device {}", uuid, id);
        
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
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
        peripheral.subscribe(&characteristic).await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Subscription failed: {}", e),
            })?;
        
        debug!("Successfully subscribed to characteristic {}", uuid);
        Ok(())
    }

    async fn unsubscribe(
        &self,
        id: &DeviceId,
        uuid: &CharacteristicUuid,
    ) -> Result<()> {
        debug!("Unsubscribing from notifications for characteristic {} on device {}", uuid, id);
        
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
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
        peripheral.unsubscribe(&characteristic).await
            .map_err(|e| Error::BackendError {
                backend: BackendKind::Btleplug,
                message: format!("Unsubscribe failed: {}", e),
            })?;
        
        debug!("Successfully unsubscribed from characteristic {}", uuid);
        Ok(())
    }

    async fn notifications(&self, id: &DeviceId) -> Result<NotificationStream> {
        // Get the peripheral and set up notification stream
        let entry = self.devices.get(id).ok_or_else(|| Error::DeviceNotFound(id.clone()))?;
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
