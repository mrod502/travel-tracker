//! Event types for device monitoring and notifications.

use crate::types::{BluetoothDevice, DeviceId, ValueNotification};

/// Fields that can be updated in a device event.
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceEvent {
    /// New device discovered.
    ///
    /// A backend that re-synthesizes discoveries when an event stream opens
    /// (BlueZ does) emits this for devices the consumer may already have
    /// recorded. Consumers decide what a repeat sighting is worth; see
    /// [`DeviceEvent::Advertisement`] for the same device heard again without
    /// anything about it moving.
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
        /// The fields that changed, in the order
        /// [`changed_device_fields`] reports them. Never empty: a report where
        /// nothing moved is an [`Advertisement`](Self::Advertisement), not this.
        changed_fields: Vec<UpdateField>,
    },

    /// The radio reported this device again and nothing tracked about it moved.
    ///
    /// This is presence evidence and nothing else: the same device, heard again,
    /// with the same name, signal strength and advertisement contents. It exists
    /// because the alternative is swallowing the report, and a swallowed report
    /// is indistinguishable at the consumer from the device having left the
    /// room. That is how a stationary beacon — stable RSSI, unchanged payload —
    /// produced exactly one sighting and then silence
    /// ([GAP_ANALYSIS B14](https://example.invalid/GAP_ANALYSIS.md#81-blocking)).
    ///
    /// A backend's job is to deliver what the radio heard; deciding whether a
    /// sighting deserves a record is the consumer's policy, and a consumer that
    /// ignores this variant is choosing to sample devices by change alone.
    Advertisement {
        /// The device as the radio reported it now.
        device: BluetoothDevice,
    },
}

/// Fields that can change in a device update.
///
/// The variants name what a backend can actually observe moving between two
/// reports of one device. There is no variant for the advertised service *list*:
/// [`BluetoothDevice`] has no field to hold it, so no backend can report a change
/// in it (a list read out of `service_data` keys is reported as
/// [`ServiceData`](Self::ServiceData), which is a different datum — see
/// [`BluetoothDevice::service_data`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UpdateField {
    /// Device name changed.
    Name,
    /// RSSI value changed.
    Rssi,
    /// Services resolved state changed.
    ServicesResolved,
    /// Connection state changed.
    Connected,
    /// The advertisement's manufacturer data changed (AD type 0xFF).
    ManufacturerData,
    /// The advertisement's service data changed.
    ServiceData,
    /// The undecoded advertisement bytes changed.
    ///
    /// Only backends that hand over [`BluetoothDevice::raw_payload`] can report
    /// this; for the others the field never moves, so it never appears.
    RawPayload,
}

/// What moved between two reports of the same device.
///
/// One function for every backend, so that "changed" means the same thing
/// whichever radio produced the report — a node sampling two adapters must not
/// get two different definitions of an update. The order is stable and is the
/// order [`DeviceEvent::DeviceUpdated`] reports.
///
/// Deliberately absent: [`BluetoothDevice::id`] and
/// [`BluetoothDevice::address`] identify the device rather than describe it, so
/// a report whose address differs is a different device, not an update.
pub fn changed_device_fields(old: &BluetoothDevice, new: &BluetoothDevice) -> Vec<UpdateField> {
    let mut changed = Vec::new();
    if old.name != new.name {
        changed.push(UpdateField::Name);
    }
    if old.rssi != new.rssi {
        changed.push(UpdateField::Rssi);
    }
    if old.services_resolved != new.services_resolved {
        changed.push(UpdateField::ServicesResolved);
    }
    if old.is_connected != new.is_connected {
        changed.push(UpdateField::Connected);
    }
    if old.manufacturer_data != new.manufacturer_data {
        changed.push(UpdateField::ManufacturerData);
    }
    if old.service_data != new.service_data {
        changed.push(UpdateField::ServiceData);
    }
    if old.raw_payload != new.raw_payload {
        changed.push(UpdateField::RawPayload);
    }
    changed
}

/// The event warranted by a report about a device the monitor already knows.
///
/// [`DeviceUpdated`](DeviceEvent::DeviceUpdated) naming what moved, or
/// [`Advertisement`](DeviceEvent::Advertisement) when nothing did. The report is
/// never dropped: see [`DeviceEvent::Advertisement`] for why swallowing it is
/// what made a stationary device invisible.
pub fn report_event(old: &BluetoothDevice, reported: BluetoothDevice) -> DeviceEvent {
    let changed_fields = changed_device_fields(old, &reported);
    if changed_fields.is_empty() {
        DeviceEvent::Advertisement { device: reported }
    } else {
        DeviceEvent::DeviceUpdated {
            device: reported,
            changed_fields,
        }
    }
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
pub type DeviceEventStream =
    std::pin::Pin<Box<dyn futures::stream::Stream<Item = DeviceEvent> + Send + 'static>>;

/// Default capacity for the channel bridging a backend's event source and
/// consumers of [`DeviceEventStream`].
pub const DEVICE_EVENT_CHANNEL_CAPACITY: usize = 256;

/// Type alias for notification streams.
pub type NotificationStream =
    std::pin::Pin<Box<dyn futures::stream::Stream<Item = ValueNotification> + Send + 'static>>;

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

        let event = DeviceEvent::DeviceAdded {
            device: device.clone(),
        };

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
        let char_uuid =
            CharacteristicUuid::parse_str("00002a00-0000-1000-8000-00805f9b34fb").unwrap();
        let notification = ValueNotification::new(char_uuid, vec![1, 2, 3]);

        let event = NotificationEvent::new(device_id.clone(), notification);

        assert_eq!(event.device_id, device_id);
        assert_eq!(event.notification.as_slice(), &[1, 2, 3]);
    }
}
