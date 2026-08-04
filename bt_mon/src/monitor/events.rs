//! Event types for device monitoring and notifications.

use crate::types::{BluetoothDevice, DeviceId, ValueNotification};

/// Fields that can be updated in a device event.
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceEvent {
    /// New device discovered.
    DeviceAdded {
        /// The discovered device.
        device: BluetoothDevice,
    },

    /// Device removed from range.
    DeviceRemoved {
        /// The ID of the removed device.
        id: DeviceId,
    },

    /// Device properties updated.
    DeviceUpdated {
        /// The updated device.
        device: BluetoothDevice,
        /// List of fields that changed.
        changed_fields: Vec<UpdateField>,
    },
}

/// Fields that can change in a device update.
#[derive(Clone, Debug, PartialEq)]
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

/// Event for a characteristic value notification.
#[derive(Clone, Debug)]
pub struct NotificationEvent {
    /// The device that sent the notification.
    pub device_id: DeviceId,
    /// The notification data.
    pub notification: ValueNotification,
}

impl NotificationEvent {
    /// Create a new notification event.
    pub fn new(device_id: DeviceId, notification: ValueNotification) -> Self {
        Self {
            device_id,
            notification,
        }
    }
}

/// Type alias for device event streams.
pub type DeviceEventStream = std::pin::Pin<
    Box<dyn futures::stream::Stream<Item = DeviceEvent> + Send + 'static>,
>;

/// Type alias for notification streams.
pub type NotificationStream = std::pin::Pin<
    Box<dyn futures::stream::Stream<Item = ValueNotification> + Send + 'static>,
>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_event_added() {
        let device = BluetoothDevice::new(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            "AA:BB:CC:DD:EE:FF".to_string(),
        )
        .with_name("Test Device");
        
        let event = DeviceEvent::DeviceAdded { device: device.clone() };
        
        match event {
            DeviceEvent::DeviceAdded { device } => {
                assert_eq!(device.name, Some("Test Device".to_string()));
            }
            _ => panic!("Wrong event variant"),
        }
    }

    #[test]
    fn test_device_event_removed() {
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        let event = DeviceEvent::DeviceRemoved { id: id.clone() };
        
        match event {
            DeviceEvent::DeviceRemoved { id } => {
                assert_eq!(id.as_str(), "AA:BB:CC:DD:EE:FF");
            }
            _ => panic!("Wrong event variant"),
        }
    }

    #[test]
    fn test_device_event_updated() {
        let device = BluetoothDevice::new(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            "AA:BB:CC:DD:EE:FF".to_string(),
        );
        let event = DeviceEvent::DeviceUpdated {
            device,
            changed_fields: vec![UpdateField::Rssi, UpdateField::Name],
        };
        
        match event {
            DeviceEvent::DeviceUpdated { changed_fields, .. } => {
                assert_eq!(changed_fields.len(), 2);
                assert!(changed_fields.contains(&UpdateField::Rssi));
                assert!(changed_fields.contains(&UpdateField::Name));
            }
            _ => panic!("Wrong event variant"),
        }
    }

    #[test]
    fn test_notification_event() {
        use crate::types::CharacteristicUuid;
        let device_id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        let char_uuid = CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
        let notification = ValueNotification::new(char_uuid, vec![1, 2, 3]);
        
        let event = NotificationEvent::new(device_id.clone(), notification);
        
        assert_eq!(event.device_id, device_id);
        assert_eq!(event.notification.as_slice(), &[1, 2, 3]);
    }
}
