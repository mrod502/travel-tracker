//! Event types for device monitoring and notifications.
//!
//! Two things live here: what a report *means* ([`report_event`],
//! [`changed_device_fields`]) and the fan-out that carries it to consumers
//! ([`DeviceEventSinks`], [`NotificationHub`]). Every backend uses the same pair,
//! so "changed" and "heard again" mean one thing whichever radio produced them.

use crate::types::{BluetoothDevice, DeviceId, ValueNotification};
use dashmap::DashMap;
use futures::StreamExt;
use log::{debug, warn};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, watch};

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

/// Capacity of the per-device channel behind [`NotificationHub`].
pub const NOTIFICATION_CHANNEL_CAPACITY: usize = 256;

/// Every open consumer of one backend's device events, with the drops counted.
///
/// A backend reads its radio in one place and [`publish`](Self::publish)es there;
/// each caller of
/// [`DeviceMonitor::device_events`](crate::monitor::DeviceMonitor::device_events)
/// gets a registration whose stream ends — and whose registration is forgotten —
/// when the stream is dropped. That is what lets a monitor publish from anywhere
/// (a discovery off the radio, a connection the node itself opened) without every
/// call site holding a channel.
///
/// The channel stays lossy on purpose: a radio pump must never block behind a slow
/// consumer. What used to be wrong is that it fired in silence, so every drop is
/// counted by [`dropped_events`](Self::dropped_events) and the first one plus one
/// in every [`DEVICE_EVENT_CHANNEL_CAPACITY`] is logged at warn level.
///
/// Cloning yields another handle to the same registry, which is how a backend
/// hands one to its pump task and keeps one for itself.
#[derive(Clone)]
pub struct DeviceEventSinks {
    inner: Arc<SinkRegistry>,
}

struct SinkRegistry {
    senders: DashMap<u64, futures::channel::mpsc::Sender<DeviceEvent>>,
    next_id: AtomicU64,
    dropped: AtomicU64,
    /// Latest subscriber count. A `watch` rather than a `Notify` so a pump asking
    /// "has everyone gone?" cannot miss a transition that happened while it was
    /// busy with a platform event.
    count_tx: watch::Sender<usize>,
}

impl Default for DeviceEventSinks {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceEventSinks {
    /// An empty registry with no consumers.
    pub fn new() -> Self {
        let (count_tx, _) = watch::channel(0usize);
        Self {
            inner: Arc::new(SinkRegistry {
                senders: DashMap::new(),
                next_id: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
                count_tx,
            }),
        }
    }

    /// Registers a consumer and returns its stream.
    ///
    /// The registration lives exactly as long as the stream: dropping the stream
    /// removes the sender and wakes anyone waiting on
    /// [`wait_until_empty`](Self::wait_until_empty), which is how a pump started
    /// for this consumer is released without waiting for the next radio event.
    pub fn subscribe(&self) -> DeviceEventStream {
        self.subscribe_seeded(std::iter::empty())
    }

    /// Registers a consumer that starts its stream with `replay`, then continues live.
    ///
    /// A stream opened after the radio has been reporting is otherwise blind: the
    /// events went to whoever was listening at the time. Each backend replays the
    /// devices it has cached, which is the same courtesy BlueZ extends when its own
    /// event stream opens and synthesizes discoveries for devices the daemon already
    /// knows.
    pub fn subscribe_seeded(
        &self,
        replay: impl IntoIterator<Item = DeviceEvent>,
    ) -> DeviceEventStream {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (mut tx, rx) =
            futures::channel::mpsc::channel::<DeviceEvent>(DEVICE_EVENT_CHANNEL_CAPACITY);

        for event in replay {
            if tx.try_send(event).is_err() {
                // Full, or already unreadable: this consumer cannot be brought further
                // up to date without blocking the caller, which is not a trade worth
                // making on someone else's stream.
                self.inner.count_drop(id);
                break;
            }
        }

        self.inner.senders.insert(id, tx);
        self.inner.update_count();

        let guard = SinkGuard {
            registry: self.inner.clone(),
            id,
        };
        Box::pin(futures::stream::unfold(
            (rx, guard),
            |(mut rx, guard)| async move {
                match rx.next().await {
                    Some(event) => Some((event, (rx, guard))),
                    None => {
                        drop(guard);
                        None
                    }
                }
            },
        ))
    }

    /// Delivers `event` to every consumer that will have it.
    pub fn publish(&self, event: &DeviceEvent) {
        if self.inner.senders.is_empty() {
            return;
        }

        // Removal happens after the loop: `iter_mut` holds the shard locks, and
        // `remove` on the same shard would deadlock against it.
        let mut dead = Vec::new();
        for mut entry in self.inner.senders.iter_mut() {
            match entry.value_mut().try_send(event.clone()) {
                Ok(()) => {}
                Err(err) if err.is_disconnected() => dead.push(*entry.key()),
                Err(_) => self.inner.count_drop(*entry.key()),
            }
        }

        if !dead.is_empty() {
            for id in dead {
                self.inner.senders.remove(&id);
            }
            self.inner.update_count();
        }
    }

    /// How many consumers are registered.
    pub fn subscriber_count(&self) -> usize {
        self.inner.senders.len()
    }

    /// Events dropped because a consumer's channel was full, across all
    /// consumers since the registry was created.
    pub fn dropped_events(&self) -> u64 {
        self.inner.dropped.load(Ordering::SeqCst)
    }

    /// Resolves once no consumer is registered, immediately if none is.
    pub async fn wait_until_empty(&self) {
        let mut rx = self.inner.count_tx.subscribe();
        loop {
            if *rx.borrow_and_update() == 0 {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Resolves once at least one consumer is registered, immediately if one already is.
    ///
    /// The other half of [`Self::wait_until_empty`], and the reason a platform pump can
    /// hold its subscription only while somebody is reading: a monitor whose consumers
    /// have all gone should stop paying for the radio rather than keep a D-Bus
    /// subscription open for an audience of none.
    pub async fn wait_until_subscribed(&self) {
        let mut rx = self.inner.count_tx.subscribe();
        loop {
            if *rx.borrow_and_update() > 0 {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

impl SinkRegistry {
    fn update_count(&self) {
        let count = self.senders.len();
        if *self.count_tx.borrow() != count {
            // `send_replace`, not `send`: a `watch` reports failure once it has no
            // receivers, and a registry has none until a pump asks to wait — which is
            // exactly the moment a monitor is empty. `send` would leave the stored
            // value stale and the next `wait_until_empty` would answer for the wrong
            // state.
            let _ = self.count_tx.send_replace(count);
        }
    }

    fn count_drop(&self, sink: u64) {
        let total = self.dropped.fetch_add(1, Ordering::SeqCst) + 1;
        if total == 1 || total.is_multiple_of(DEVICE_EVENT_CHANNEL_CAPACITY as u64) {
            warn!(
                "device event channel full for subscriber {sink}; {total} events dropped so far \
                 — a consumer is not keeping up with the radio"
            );
        } else {
            debug!("device event channel full for subscriber {sink}; dropped event");
        }
    }
}

/// Forgets a consumer's registration when its stream goes away.
struct SinkGuard {
    registry: Arc<SinkRegistry>,
    id: u64,
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        self.registry.senders.remove(&self.id);
        self.registry.update_count();
    }
}

/// Fans characteristic notifications out to the consumers of one device.
///
/// `GattClient::notifications` takes a device and multiplexes every characteristic
/// subscribed on it, so a backend needs one place that holds a device's frames and
/// N readers. Without it each backend improvised, and both improvisations were
/// silent failures: `btleplug` read the platform stream and threw it away, the mock
/// kept one sender per device so a second subscriber closed the first.
///
/// A device's channel is created by its first subscriber and closed by
/// [`forget`](Self::forget) (a disconnect, a stopped monitor), which ends every
/// stream on it. [`wait_until_empty`](Self::wait_until_empty) tells the backend
/// when the last consumer of a device has gone, so the platform read serving them
/// can be stopped rather than left running for nobody.
#[derive(Clone)]
pub struct NotificationHub {
    channels: Arc<DashMap<DeviceId, Arc<DeviceChannel>>>,
}

struct DeviceChannel {
    sender: broadcast::Sender<ValueNotification>,
    subscribers: AtomicU64,
    dropped: AtomicU64,
    empty_tx: watch::Sender<u64>,
    /// Fires when the hub lets the device go, ending every stream on it.
    ///
    /// A stream's guard holds an `Arc` to this channel, so removing the hub's entry
    /// cannot drop the broadcast sender on its own — the sender outlives the map
    /// entry as long as any consumer is reading, and those consumers would wait for a
    /// frame that will never come. The notification is what closes them.
    closed_tx: watch::Sender<bool>,
}

impl DeviceChannel {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY);
        let (empty_tx, _) = watch::channel(0u64);
        let (closed_tx, _) = watch::channel(false);
        Self {
            sender,
            subscribers: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            empty_tx,
            closed_tx,
        }
    }

    fn set_count(&self) {
        let count = self.subscribers.load(Ordering::SeqCst);
        if *self.empty_tx.borrow() != count {
            // `send_replace` for the same reason as the event registry's counter:
            // `send` fails when nobody is watching yet, and a channel that has just
            // been created has nobody watching yet — which is when the value that
            // the first `wait_until_empty` reads has to be right.
            let _ = self.empty_tx.send_replace(count);
        }
    }
}

impl Default for NotificationHub {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationHub {
    /// An empty hub: no device has a channel until something subscribes.
    pub fn new() -> Self {
        Self {
            channels: Arc::new(DashMap::new()),
        }
    }

    /// Opens a stream for `device_id`, creating its channel on first use.
    ///
    /// Every subscriber of a device receives every frame published on it; frames
    /// that arrive before this is called are not replayed, which matches a radio:
    /// a notification is a moment, not state.
    pub fn subscribe(&self, device_id: &DeviceId) -> NotificationStream {
        let channel = self
            .channels
            .entry(device_id.clone())
            .or_insert_with(|| Arc::new(DeviceChannel::new()))
            .clone();

        let rx = channel.sender.subscribe();
        let closed = channel.closed_tx.subscribe();
        channel.subscribers.fetch_add(1, Ordering::SeqCst);
        channel.set_count();

        let guard = NotificationGuard { channel };
        Box::pin(futures::stream::unfold(
            (rx, closed, guard),
            |(mut rx, mut closed, guard)| async move {
                loop {
                    tokio::select! {
                        // Biased toward the close signal: once the hub has let the
                        // device go, the frames still sitting in this consumer's
                        // queue are not live data and should not be delivered as
                        // though they were.
                        biased;
                        _ = closed.changed() => {
                            drop(guard);
                            return None;
                        }
                        received = rx.recv() => match received {
                            Ok(notification) => return Some((notification, (rx, closed, guard))),
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                guard.count_dropped(skipped);
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                drop(guard);
                                return None;
                            }
                        },
                    }
                }
            },
        ))
    }

    /// Delivers one frame to every consumer of `device_id`, and says how many got
    /// it. Zero means nobody was listening, which is not an error: a device can be
    /// notifying while the node has no stream open.
    pub fn publish(&self, device_id: &DeviceId, notification: ValueNotification) -> usize {
        let Some(channel) = self.channels.get(device_id) else {
            return 0;
        };
        channel.sender.send(notification).unwrap_or(0)
    }

    /// Consumers currently reading `device_id`.
    pub fn subscriber_count(&self, device_id: &DeviceId) -> u64 {
        self.channels
            .get(device_id)
            .map(|entry| entry.value().subscribers.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// Frames this hub dropped for `device_id` because a consumer fell behind.
    pub fn dropped_count(&self, device_id: &DeviceId) -> u64 {
        self.channels
            .get(device_id)
            .map(|entry| entry.value().dropped.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// Resolves once `device_id` has no consumers, immediately if it never had any.
    pub async fn wait_until_empty(&self, device_id: &DeviceId) {
        let Some(channel) = self.channels.get(device_id) else {
            return;
        };
        let mut rx = channel.empty_tx.subscribe();
        drop(channel);

        loop {
            if *rx.borrow_and_update() == 0 {
                return;
            }
            if rx.changed().await.is_err() {
                // The channel is gone, which is emptier than empty.
                return;
            }
        }
    }

    /// Closes `device_id`'s channel, ending every stream on it.
    ///
    /// The next subscriber starts a fresh channel, so this is what a backend calls
    /// when a device disconnects or the monitor shuts down. The subscriber count is
    /// left alone on purpose: each open stream still decrements it as its guard
    /// drops, so a pump waiting on [`wait_until_empty`](Self::wait_until_empty)
    /// wakes when the last one goes, rather than on a count forced to zero here.
    pub fn forget(&self, device_id: &DeviceId) {
        // Remove first, so a `publish` racing with this either lands in the channel
        // before it leaves the map or finds no entry at all; then close the streams
        // that outlived it.
        if let Some((_, channel)) = self.channels.remove(device_id) {
            channel.closed_tx.send_replace(true);
        }
    }

    /// Closes every device's channel. A monitor's `Drop` calls this so a discarded
    /// monitor cannot leave notification streams open on state nobody owns any more.
    pub fn forget_all(&self) {
        let ids: Vec<DeviceId> = self
            .channels
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for id in ids {
            self.forget(&id);
        }
    }
}

/// Counts a consumer's lag and releases its slot on the device's channel.
struct NotificationGuard {
    channel: Arc<DeviceChannel>,
}

impl NotificationGuard {
    fn count_dropped(&self, skipped: u64) {
        let total = self.channel.dropped.fetch_add(skipped, Ordering::SeqCst) + skipped;
        warn!(
            "notification channel full; {skipped} frame(s) skipped, {total} dropped in total — \
             a consumer is not keeping up with the notifications"
        );
    }
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        self.channel.subscribers.fetch_sub(1, Ordering::SeqCst);
        self.channel.set_count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CharacteristicUuid, ServiceUuid};
    use std::time::Duration;

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

    fn beacon_event(rssi: i32) -> DeviceEvent {
        DeviceEvent::Advertisement {
            device: beacon().with_rssi(rssi),
        }
    }

    fn rssi_of_each(events: &[DeviceEvent]) -> Vec<i32> {
        events
            .iter()
            .map(|event| match event {
                DeviceEvent::Advertisement { device } => device.rssi.unwrap_or_default(),
                other => panic!("expected an advertisement, got {other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn each_subscriber_sees_each_event_exactly_once() {
        let sinks = DeviceEventSinks::new();
        let mut first = sinks.subscribe();
        let mut second = sinks.subscribe();
        assert_eq!(sinks.subscriber_count(), 2);

        for rssi in [-50, -51, -52] {
            sinks.publish(&beacon_event(rssi));
        }

        for stream in [&mut first, &mut second] {
            let mut heard = Vec::new();
            for _ in 0..3 {
                let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
                    .await
                    .expect("the event should already be queued")
                    .expect("stream open");
                heard.push(event);
            }
            assert_eq!(rssi_of_each(&heard), vec![-50, -51, -52]);
        }
    }

    #[tokio::test]
    async fn a_saturated_consumer_loses_events_and_the_loss_is_counted() {
        // The lossy channel is a decision; not being able to tell that it fired is
        // the defect. A consumer that never reads must produce a number, not
        // silence, and must not cost the alert consumer anything.
        let sinks = DeviceEventSinks::new();
        let sleeping = sinks.subscribe();
        let mut awake = sinks.subscribe();

        let total = DEVICE_EVENT_CHANNEL_CAPACITY + 5;
        for rssi in 0..total as i32 {
            sinks.publish(&beacon_event(-40 - rssi));
        }

        // futures' bounded `mpsc` keeps one item in a slot ahead of its queue, so a
        // subscriber that never reads absorbs capacity + 1 and sheds the rest. Two
        // subscribers, neither reading, shed twice that — and the registry can say so,
        // which is the whole point: the lossy channel is the design, the silence
        // around it was the defect.
        let overflow = (total - DEVICE_EVENT_CHANNEL_CAPACITY - 1) as u64;
        assert_eq!(
            sinks.dropped_events(),
            overflow * 2,
            "one channel holds {} plus one, and the run overflows both by {overflow}",
            DEVICE_EVENT_CHANNEL_CAPACITY
        );

        // The other consumer still has the whole run, oldest first.
        let first = tokio::time::timeout(Duration::from_secs(1), awake.next())
            .await
            .expect("queued")
            .expect("stream open");
        assert!(matches!(first, DeviceEvent::Advertisement { .. }));
        assert_eq!(
            sinks.dropped_events(),
            overflow * 2,
            "reading what survived does not undo what was lost"
        );

        // Keep the sleeping stream alive so its sender is not pruned before the
        // assertion above runs.
        drop(sleeping);
    }

    #[tokio::test]
    async fn dropping_a_stream_frees_the_registry_without_waiting_for_an_event() {
        let sinks = DeviceEventSinks::new();
        let stream = sinks.subscribe();
        assert_eq!(sinks.subscriber_count(), 1);

        drop(stream);
        assert_eq!(sinks.subscriber_count(), 0);

        // A pump parked on this would otherwise linger until the next radio event.
        tokio::time::timeout(Duration::from_secs(1), sinks.wait_until_empty())
            .await
            .expect("the registry reports empty as soon as the last stream is dropped");
    }

    #[tokio::test]
    async fn wait_until_empty_ignores_the_time_before_anyone_subscribed() {
        let sinks = DeviceEventSinks::new();
        tokio::time::timeout(Duration::from_secs(1), sinks.wait_until_empty())
            .await
            .expect("an empty registry is already empty");
    }

    #[tokio::test]
    async fn every_notification_subscriber_gets_every_frame() {
        // The bug this replaces: the mock keyed its senders by device, so a second
        // `notifications()` call closed the first caller's stream.
        let hub = NotificationHub::new();
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        let characteristic =
            CharacteristicUuid::parse_str("00002a37-0000-1000-8000-00805f9b34fb").unwrap();

        let mut first = hub.subscribe(&id);
        let mut second = hub.subscribe(&id);
        assert_eq!(hub.subscriber_count(&id), 2);

        for byte in [1u8, 2, 3] {
            hub.publish(&id, ValueNotification::new(characteristic, vec![byte]));
        }

        for stream in [&mut first, &mut second] {
            for byte in [1u8, 2, 3] {
                let frame = tokio::time::timeout(Duration::from_secs(1), stream.next())
                    .await
                    .expect("the frame should already be queued")
                    .expect("stream open");
                assert_eq!(frame.as_slice(), &[byte]);
                assert_eq!(frame.characteristic, characteristic);
                assert!(frame.timestamp.is_some(), "stamped on receipt");
            }
        }
    }

    #[tokio::test]
    async fn dropping_the_last_notification_stream_lets_the_backend_stop_reading() {
        let hub = NotificationHub::new();
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        hub.wait_until_empty(&id).await;

        let first = hub.subscribe(&id);
        let second = hub.subscribe(&id);
        assert_eq!(hub.subscriber_count(&id), 2);

        let waiter = {
            let hub = hub.clone();
            let id = id.clone();
            tokio::spawn(async move {
                hub.wait_until_empty(&id).await;
            })
        };

        drop(first);
        assert_eq!(hub.subscriber_count(&id), 1);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect_err("one consumer is left, so the hub is not empty yet");

        drop(second);
        tokio::time::timeout(Duration::from_secs(1), hub.wait_until_empty(&id))
            .await
            .expect("the last consumer went, so the platform read can stop");
    }

    #[tokio::test]
    async fn forgetting_a_device_ends_its_streams() {
        let hub = NotificationHub::new();
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        let mut stream = hub.subscribe(&id);

        hub.forget(&id);

        assert!(
            tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("the stream ends rather than hanging")
                .is_none(),
            "a disconnected device's notification stream has to close"
        );
    }

    #[tokio::test]
    async fn a_consumer_that_falls_behind_counts_the_frames_it_missed() {
        let hub = NotificationHub::new();
        let id = DeviceId::new("AA:BB:CC:DD:EE:FF");
        let characteristic =
            CharacteristicUuid::parse_str("00002a37-0000-1000-8000-00805f9b34fb").unwrap();
        let mut stream = hub.subscribe(&id);

        let published = NOTIFICATION_CHANNEL_CAPACITY + 20;
        for byte in 0..published {
            hub.publish(
                &id,
                ValueNotification::new(characteristic, vec![byte as u8]),
            );
        }

        // The first read reports the lag instead of quietly re-syncing.
        let mut frames = Vec::new();
        while let Ok(Some(frame)) =
            tokio::time::timeout(Duration::from_millis(100), stream.next()).await
        {
            frames.push(frame);
        }

        assert!(
            hub.dropped_count(&id) >= 1,
            "a consumer that missed frames must be able to say so"
        );
        assert_eq!(
            frames.last().expect("the newest frame survives").as_slice(),
            &[(published - 1) as u8],
            "the live end of the stream is still delivered"
        );
    }
}
