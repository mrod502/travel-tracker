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
    /// produced exactly one sighting and then silence (GAP_ANALYSIS B14).
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
    use crate::types::ServiceUuid;

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

    fn beacon() -> BluetoothDevice {
        BluetoothDevice::new(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            "AA:BB:CC:DD:EE:FF".to_string(),
        )
        .with_name("Stationary Beacon")
        .with_rssi(-70)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02])
    }

    #[test]
    fn a_report_where_nothing_moved_is_still_a_sighting() {
        // The defect this pins: a repeat report of an unchanged device used to
        // produce no event at all, and "no event" is indistinguishable from "the
        // device left". A stationary beacon therefore yielded one sighting and
        // then silence.
        let old = beacon();
        let reported = beacon();

        assert_eq!(changed_device_fields(&old, &reported), Vec::new());
        assert_eq!(
            report_event(&old, reported.clone()),
            DeviceEvent::Advertisement { device: reported }
        );
    }

    #[test]
    fn every_tracked_field_moves_a_report_on_its_own() {
        // Each variant of `UpdateField` must be reachable from exactly the one
        // datum that owns it, or a consumer cannot tell what the radio changed.
        type FieldCase = (
            &'static str,
            UpdateField,
            fn(BluetoothDevice) -> BluetoothDevice,
        );

        let cases: Vec<FieldCase> = vec![
            ("name", UpdateField::Name, |d| d.with_name("Renamed")),
            ("rssi", UpdateField::Rssi, |d| d.with_rssi(-51)),
            (
                "services_resolved",
                UpdateField::ServicesResolved,
                |mut d| {
                    d.services_resolved = true;
                    d
                },
            ),
            ("is_connected", UpdateField::Connected, |d| {
                d.with_connected(true)
            }),
            ("manufacturer_data", UpdateField::ManufacturerData, |d| {
                d.with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03])
            }),
            ("service_data", UpdateField::ServiceData, |d| {
                d.with_service_data(
                    ServiceUuid::parse_str("0000fd87-0000-1000-8000-00805f9b34fb")
                        .expect("a service uuid"),
                    vec![0xAA],
                )
            }),
            ("raw_payload", UpdateField::RawPayload, |d| {
                d.with_raw_payload([0x01, 0x09, 0xFF])
            }),
        ];

        for (column, expected, change) in cases {
            let reported = change(beacon());
            assert_eq!(
                changed_device_fields(&beacon(), &reported),
                vec![expected],
                "changing {column} should report exactly {expected:?}"
            );
            assert_eq!(
                report_event(&beacon(), reported.clone()),
                DeviceEvent::DeviceUpdated {
                    device: reported,
                    changed_fields: vec![expected],
                },
                "a lone {column} change should be an update naming it"
            );
        }
    }

    #[test]
    fn a_report_names_every_field_that_moved_in_one_packet() {
        let uuid = ServiceUuid::parse_str("0000fd87-0000-1000-8000-00805f9b34fb").unwrap();
        let reported = beacon()
            .with_rssi(-44)
            .with_manufacturer_data(0x004C, vec![0x09])
            .with_service_data(uuid, vec![0x01]);

        // The order is the function's declaration order, not the order the
        // fields were touched: consumers may compare `changed_fields` directly.
        assert_eq!(
            changed_device_fields(&beacon(), &reported),
            vec![
                UpdateField::Rssi,
                UpdateField::ManufacturerData,
                UpdateField::ServiceData,
            ]
        );
    }

    #[test]
    fn manufacturer_data_that_only_rearrives_is_not_a_change() {
        let mut old = beacon();
        old.manufacturer_data
            .insert(0x00E0, vec![0x10, 0x11, 0x12, 0x13]);

        // Same two companies, same bytes, inserted in the other order.
        let mut reported = beacon();
        reported
            .manufacturer_data
            .insert(0x00E0, vec![0x10, 0x11, 0x12, 0x13]);
        reported.manufacturer_data.insert(0x004C, vec![0x01, 0x02]);

        let expected = DeviceEvent::Advertisement {
            device: reported.clone(),
        };
        assert_eq!(
            report_event(&old, reported),
            expected,
            "the same advertisement re-arrived, which is presence, not news"
        );
    }

    #[test]
    fn a_manufacturer_that_stopped_advertising_is_a_change() {
        let mut old = beacon();
        old.manufacturer_data
            .insert(0x00E0, vec![0x10, 0x11, 0x12, 0x13]);

        // The report carries only the 0x004C structure: a company the previous
        // advertisement had is gone, and that is a content change.
        let reported = beacon();

        assert_eq!(
            changed_device_fields(&old, &reported),
            vec![UpdateField::ManufacturerData]
        );
    }

    #[test]
    fn an_address_is_not_a_tracked_field() {
        // `id`/`address` pick the device out; they do not describe it. A report
        // whose address differs is a different device, which the backend handles
        // as a discovery rather than an update — so the diff says nothing moved.
        let old = beacon();
        let reported = BluetoothDevice::new(
            DeviceId::new("11:22:33:44:55:66"),
            "11:22:33:44:55:66".to_string(),
        )
        .with_name("Stationary Beacon")
        .with_rssi(-70)
        .with_manufacturer_data(0x004C, vec![0x01, 0x02]);

        assert_eq!(changed_device_fields(&old, &reported), Vec::new());
    }

    #[test]
    fn a_report_carries_the_snapshot_the_radio_just_sent() {
        // The old snapshot is only there to diff against; what the consumer
        // stores has to be the new one.
        let event = report_event(
            &beacon(),
            beacon().with_rssi(-33).with_name("Now You See Me"),
        );

        let DeviceEvent::DeviceUpdated { device, .. } = event else {
            panic!("a moved field should be an update");
        };
        assert_eq!(device.rssi, Some(-33));
        assert_eq!(device.name.as_deref(), Some("Now You See Me"));
    }

    #[test]
    fn a_missing_field_becoming_present_is_a_change() {
        // `None` to `Some` is the first time the radio said anything about a
        // property, which is the most informative change there is.
        let quiet = BluetoothDevice::new(
            DeviceId::new("AA:BB:CC:DD:EE:FF"),
            "AA:BB:CC:DD:EE:FF".to_string(),
        );
        let speaking = beacon();

        assert_eq!(
            changed_device_fields(&quiet, &speaking),
            vec![
                UpdateField::Name,
                UpdateField::Rssi,
                UpdateField::ManufacturerData,
            ]
        );
        // And the other way round: a property that went quiet also moved.
        assert_eq!(
            changed_device_fields(&speaking, &quiet),
            vec![
                UpdateField::Name,
                UpdateField::Rssi,
                UpdateField::ManufacturerData,
            ]
        );
    }
}
