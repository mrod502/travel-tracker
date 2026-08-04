//! bluer backend implementation for bt_mon.
//!
//! This module provides the Linux/BlueZ implementation using the `bluer` crate.
//! Note: This backend is Linux-only but provides additional features like
//! GATT server support and BLE advertising.

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::monitor::events::{DeviceEventStream, NotificationStream};
use crate::monitor::{DeviceMonitor, GattClient};
use crate::types::{
    BluetoothDevice, CharacteristicUuid, DeviceId, GattService, ValueNotification,
};

/// bluer backend implementation of DeviceMonitor and GattClient.
pub struct BluerMonitor {
    // Placeholder for actual bluer adapter
    // Will be filled in during Phase 3 implementation
    _placeholder: std::marker::PhantomData<()>,
}

impl BluerMonitor {
    /// Create a new bluer monitor.
    ///
    /// # Errors
    ///
    /// Returns an error if the BlueZ session cannot be established.
    pub async fn new() -> Result<Self> {
        // Phase 3 implementation will go here:
        // - Create bluer::Session
        // - Get default adapter
        // - Set up event channels
        // - Initialize device cache

        #[cfg(feature = "bluer")]
        {
            // TODO: Actual implementation
            // let session = bluer::Session::new().await?;
            // let adapter = session.default_adapter()?;

            Ok(Self {
                _placeholder: std::marker::PhantomData,
            })
        }

        #[cfg(not(feature = "bluer"))]
        {
            Err(Error::BackendUnavailable {
                required: BackendKind::Bluer,
                available: vec![],
            })
        }
    }
}

#[async_trait]
impl DeviceMonitor for BluerMonitor {
    async fn start_scan(&self) -> Result<()> {
        // Phase 3 implementation
        Err(Error::Internal("Not yet implemented".to_string()))
    }

    async fn stop_scan(&self) -> Result<()> {
        // Phase 3 implementation
        Err(Error::Internal("Not yet implemented".to_string()))
    }

    async fn devices(&self) -> Result<Vec<BluetoothDevice>> {
        // Phase 3 implementation
        Ok(vec![])
    }

    async fn device(&self, id: &DeviceId) -> Result<BluetoothDevice> {
        // Phase 3 implementation
        Err(Error::DeviceNotFound(id.clone()))
    }

    async fn is_powered(&self) -> Result<bool> {
        // Phase 3 implementation
        Ok(false)
    }

    async fn adapter_info(&self) -> Result<String> {
        // Phase 3 implementation
        Ok("bluer backend".to_string())
    }

    async fn device_events(&self) -> Result<DeviceEventStream> {
        // Phase 3 implementation
        use futures::stream::{self, Stream};
        let stream = stream::empty::<crate::monitor::DeviceEvent>();
        Ok(Box::pin(stream))
    }

    async fn is_scanning(&self) -> Result<bool> {
        // Phase 3 implementation
        Ok(false)
    }
}

#[async_trait]
impl GattClient for BluerMonitor {
    async fn connect(&self, _id: &DeviceId) -> Result<()> {
        // Phase 3 implementation
        Err(Error::Internal("Not yet implemented".to_string()))
    }

    async fn disconnect(&self, _id: &DeviceId) -> Result<()> {
        // Phase 3 implementation
        Err(Error::Internal("Not yet implemented".to_string()))
    }

    async fn is_connected(&self, _id: &DeviceId) -> Result<bool> {
        // Phase 3 implementation
        Ok(false)
    }

    async fn discover_services(&self, _id: &DeviceId) -> Result<Vec<GattService>> {
        // Phase 3 implementation
        Ok(vec![])
    }

    async fn services(&self, _id: &DeviceId) -> Result<Vec<GattService>> {
        // Phase 3 implementation
        Ok(vec![])
    }

    async fn read_characteristic(
        &self,
        _id: &DeviceId,
        _uuid: &CharacteristicUuid,
    ) -> Result<Vec<u8>> {
        // Phase 3 implementation
        Err(Error::Internal("Not yet implemented".to_string()))
    }

    async fn write_characteristic(
        &self,
        _id: &DeviceId,
        _uuid: &CharacteristicUuid,
        _value: &[u8],
        _response: bool,
    ) -> Result<()> {
        // Phase 3 implementation
        Err(Error::Internal("Not yet implemented".to_string()))
    }

    async fn subscribe(
        &self,
        _id: &DeviceId,
        _uuid: &CharacteristicUuid,
    ) -> Result<()> {
        // Phase 3 implementation
        Err(Error::Internal("Not yet implemented".to_string()))
    }

    async fn unsubscribe(
        &self,
        _id: &DeviceId,
        _uuid: &CharacteristicUuid,
    ) -> Result<()> {
        // Phase 3 implementation
        Err(Error::Internal("Not yet implemented".to_string()))
    }

    async fn notifications(&self, _id: &DeviceId) -> Result<NotificationStream> {
        // Phase 3 implementation
        use futures::stream::{self, Stream};
        let stream = stream::empty::<ValueNotification>();
        Ok(Box::pin(stream))
    }
}
