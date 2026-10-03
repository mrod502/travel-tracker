//! Core data models for Bluetooth LE advertisement observations and identity tracking.
//!
//! This module defines the fundamental data structures used throughout the identity
//! resolution engine, including advertisement observations, device identities, and
//! physical identity tracking.

use uuid::Uuid;

use crate::ad::AdStructures;
use crate::evidence::{Datum, Feature, FeatureSources};
use crate::time::ObservationTime;

/// Bluetooth address representation.
///
/// Stores the 48-bit Bluetooth MAC address along with metadata about its type.
/// This is the only place where raw Bluetooth addresses are stored internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BluetoothAddress {
    /// The 6-byte MAC address.
    pub bytes: [u8; 6],
}

impl BluetoothAddress {
    /// Creates a new Bluetooth address from bytes.
    pub fn new(bytes: [u8; 6]) -> Self {
        Self { bytes }
    }

    /// Creates a Bluetooth address from a hex string representation.
    ///
    /// Accepts formats like "00:11:22:33:44:55" or "001122334455".
    pub fn from_hex(s: &str) -> Option<Self> {
        let cleaned: String = s.chars().filter(|c| *c != ':').collect();
        if cleaned.len() != 12 {
            return None;
        }

        let mut bytes = [0u8; 6];
        for (i, chunk) in cleaned.as_bytes().chunks(2).enumerate() {
            let hex_str = std::str::from_utf8(chunk).ok()?;
            bytes[i] = u8::from_str_radix(hex_str, 16).ok()?;
        }

        Some(Self { bytes })
    }

    /// Returns the address as a byte slice.
    pub fn as_bytes(&self) -> &[u8; 6] {
        &self.bytes
    }
}

impl std::fmt::Display for BluetoothAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.bytes[0],
            self.bytes[1],
            self.bytes[2],
            self.bytes[3],
            self.bytes[4],
            self.bytes[5]
        )
    }
}

/// Type of Bluetooth address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressType {
    /// Public identity address (static).
    Public,
    /// Random static address.
    RandomStatic,
    /// Private resolvable address (changes with IRK).
    PrivateResolvable,
    /// Private non-resolvable address (changes frequently).
    PrivateNonResolvable,
}

/// Service data structure containing UUID and associated data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceData {
    /// The service UUID.
    pub uuid: Uuid,
    /// The associated service data.
    pub data: Vec<u8>,
}

/// A normalized Bluetooth LE advertisement observation.
///
/// This structure represents a single advertisement event captured from
/// the wireless medium. It abstracts away the details of how the
/// observation was obtained (BlueZ, raw HCI, pcap, a stored occurrence…).
///
/// Two fields describe *the feed* rather than the advertisement:
/// [`timestamp`](Self::timestamp) carries the wall clock alongside the monotonic
/// reading so a stored row can be replayed, and
/// [`sources`](Self.sources) records which features the producer actually had a
/// source for. Both exist so a resolver can tell "these two advertisements
/// differ" from "this feed cannot see the difference" — see
/// [`crate::evidence`].
///
/// Every `with_*` builder declares the provenance of what it sets, so an
/// observation assembled from a lossy source does not have to claim more than it
/// was given.
///
/// # Example
///
/// ```
/// use bt_iden::models::{AdvertisementObservation, AddressType, BluetoothAddress};
/// use bt_iden::time::ObservationTime;
///
/// let observation = AdvertisementObservation::new(
///     ObservationTime::now(),
///     BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
///     AddressType::PrivateResolvable,
/// )
/// .with_rssi(-65)
/// .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);
///
/// assert_eq!(observation.rssi, -65);
/// // Only what the builders were called with has a source.
/// assert_eq!(observation.sources.get(bt_iden::evidence::Feature::Rssi), bt_iden::evidence::Datum::Direct);
/// assert_eq!(observation.sources.get(bt_iden::evidence::Feature::Appearance), bt_iden::evidence::Datum::Absent);
/// ```
#[derive(Debug, Clone)]
pub struct AdvertisementObservation {
    /// When the observation was captured, as a wall clock plus the monotonic
    /// instant read alongside it when the capture had one.
    pub timestamp: ObservationTime,

    /// The Bluetooth source address.
    pub address: BluetoothAddress,

    /// The type of address (public, random, etc.).
    pub address_type: AddressType,

    /// Received Signal Strength Indicator in dBm.
    ///
    /// `0` with `sources.get(Feature::Rssi) == Datum::Absent` means *no
    /// measurement*, not a reading of 0 dBm; the scorer honours the datum, not the
    /// placeholder.
    pub rssi: i16,

    /// Whether the device advertises as connectable.
    pub connectable: bool,

    /// Manufacturer-specific data ID (16-bit).
    pub manufacturer_id: Option<u16>,

    /// Manufacturer-specific data payload.
    pub manufacturer_data: Vec<u8>,

    /// Service data records (UUID + payload).
    pub service_data: Vec<ServiceData>,

    /// List of service UUIDs advertised.
    pub service_uuids: Vec<Uuid>,

    /// Local name (shortened or complete).
    pub local_name: Option<String>,

    /// TX power level if advertised.
    pub tx_power: Option<i8>,

    /// Appearance value (device category).
    pub appearance: Option<u16>,

    /// The AD type codes as the producer read them, in transmission order.
    ///
    /// Empty when the producer never saw AD structures, which is what
    /// [`AdvertisementObservation::effective_layout`] falls back on the
    /// synthesized layout for.
    pub field_layout: Vec<u8>,

    /// Which datum each scored feature came from.
    pub sources: FeatureSources,
}

impl AdvertisementObservation {
    /// Creates a new observation with the given timestamp and address.
    ///
    /// The address and the timestamp have a direct source — they are the two
    /// things a caller cannot be wrong about — and every other feature starts
    /// absent until a builder or a direct field assignment gives it one.
    pub fn new(
        timestamp: ObservationTime,
        address: BluetoothAddress,
        address_type: AddressType,
    ) -> Self {
        let sources = FeatureSources::new()
            .with(Feature::Address, Datum::Direct)
            .with(Feature::TimeContinuity, Datum::Direct);

        Self {
            timestamp,
            address,
            address_type,
            rssi: 0,
            connectable: false,
            manufacturer_id: None,
            manufacturer_data: Vec::new(),
            service_data: Vec::new(),
            service_uuids: Vec::new(),
            local_name: None,
            tx_power: None,
            appearance: None,
            field_layout: Vec::new(),
            sources,
        }
    }

    /// Replaces the whole provenance map, for adapters that compute it themselves.
    pub fn with_sources(mut self, sources: FeatureSources) -> Self {
        self.sources = sources;
        self
    }

    /// Sets the RSSI value.
    pub fn with_rssi(mut self, rssi: i16) -> Self {
        self.rssi = rssi;
        self.sources.set(Feature::Rssi, Datum::Direct);
        self
    }

    /// Sets the connectable flag, declaring how it was obtained.
    ///
    /// A stack that reports the PDU type supplies
    /// [`Datum::Direct`]; one that only has a
    /// flags byte supplies [`Datum::Derived`],
    /// which is why this is not a plain setter.
    pub fn with_connectable(mut self, connectable: bool, datum: Datum) -> Self {
        self.connectable = connectable;
        self.sources.set(Feature::Connectable, datum);
        self
    }

    /// Sets manufacturer-specific data.
    ///
    /// Declares both the company id and the payload bytes as direct evidence: the
    /// payload feature compares the manufacturer structure's bytes, which is exactly
    /// what this hands over. A feed that only reached the payload indirectly — a
    /// stored row that kept the first manufacturer entry, or service data standing
    /// in for it — says so through
    /// [`with_manufacturer_provenance`](Self::with_manufacturer_provenance) instead.
    pub fn with_manufacturer_data(mut self, id: u16, data: Vec<u8>) -> Self {
        self.manufacturer_id = Some(id);
        self.manufacturer_data = data;
        self.sources.set(Feature::ManufacturerId, Datum::Direct);
        self.sources.set(Feature::PayloadSimilarity, Datum::Direct);
        self
    }

    /// Sets manufacturer data *and* declares the datum for both the company id and
    /// the payload, for feeds where one of the two is a stand-in (a stored row that
    /// kept only the first manufacturer entry, say).
    pub fn with_manufacturer_provenance(
        mut self,
        id: Option<u16>,
        data: Vec<u8>,
        id_datum: Datum,
        payload_datum: Datum,
    ) -> Self {
        self.manufacturer_id = id;
        self.manufacturer_data = data;
        self.sources.set(Feature::ManufacturerId, id_datum);
        self.sources.set(Feature::PayloadSimilarity, payload_datum);
        self
    }

    /// Sets the advertised service UUID list, declaring its source.
    pub fn with_service_uuids(mut self, uuids: Vec<Uuid>, datum: Datum) -> Self {
        self.service_uuids = uuids;
        self.sources.set(Feature::ServiceUuids, datum);
        self
    }

    /// Adds a service UUID, reading it as the advertised list.
    pub fn with_service_uuid(mut self, uuid: Uuid) -> Self {
        self.service_uuids.push(uuid);
        self.sources.set(Feature::ServiceUuids, Datum::Direct);
        self
    }

    /// Adds service data.
    ///
    /// Service data bytes are what the payload feature falls back to when no
    /// manufacturer structure was seen, so adding them declares the payload
    /// *derived* — but only while nothing better has already claimed the feature,
    /// so this never downgrades a manufacturer payload set earlier.
    pub fn with_service_data(mut self, uuid: Uuid, data: Vec<u8>) -> Self {
        self.service_data.push(ServiceData { uuid, data });
        if self.sources.get(Feature::PayloadSimilarity) == Datum::Absent {
            self.sources.set(Feature::PayloadSimilarity, Datum::Derived);
        }
        self
    }

    /// Sets the local name, declaring its source.
    pub fn with_local_name_datum(mut self, name: String, datum: Datum) -> Self {
        self.local_name = Some(name);
        self.sources.set(Feature::Name, datum);
        self
    }

    /// Sets the local name.
    pub fn with_local_name(self, name: String) -> Self {
        self.with_local_name_datum(name, Datum::Direct)
    }

    /// Sets the TX power level.
    pub fn with_tx_power(mut self, tx_power: i8) -> Self {
        self.tx_power = Some(tx_power);
        self
    }

    /// Sets the appearance value, declaring its source.
    pub fn with_appearance_datum(mut self, appearance: u16, datum: Datum) -> Self {
        self.appearance = Some(appearance);
        self.sources.set(Feature::Appearance, datum);
        self
    }

    /// Sets the appearance value.
    pub fn with_appearance(self, appearance: u16) -> Self {
        self.with_appearance_datum(appearance, Datum::Direct)
    }

    /// Sets the AD type codes read off the air.
    pub fn with_field_layout(mut self, layout: Vec<u8>) -> Self {
        self.sources.set(Feature::FieldLayout, Datum::Direct);
        self.field_layout = layout;
        self
    }

    /// Fills every feature that advertising-data structures can supply.
    ///
    /// This is how a feed that exposes raw advertisement bytes reaches full model
    /// coverage: the layout, appearance, name, manufacturer and advertised service
    /// list all come from the structures themselves, so they are direct evidence.
    /// Only connectability stays derived, because the authoritative answer is in
    /// the PDU type, which no host stack here exposes.
    ///
    /// Fields the structures do not contain are left exactly as they were, so an
    /// adapter can layer this on top of a partially filled observation.
    pub fn with_ad_structures(mut self, ad: &AdStructures) -> Self {
        if !ad.is_empty() {
            self.field_layout = ad.layout();
            self.sources.set(Feature::FieldLayout, Datum::Direct);
        }
        if let Some(appearance) = ad.appearance() {
            self.appearance = Some(appearance);
            self.sources.set(Feature::Appearance, Datum::Direct);
        }
        if let Some(tx_power) = ad.tx_power() {
            self.tx_power = Some(tx_power);
        }
        if let Some(name) = ad.local_name() {
            self.local_name = Some(name);
            self.sources.set(Feature::Name, Datum::Direct);
        }
        if let Some((id, payload)) = ad.manufacturer() {
            self.manufacturer_id = Some(id);
            self.manufacturer_data = payload;
            self.sources.set(Feature::ManufacturerId, Datum::Direct);
            self.sources.set(Feature::PayloadSimilarity, Datum::Direct);
        }
        let advertised = ad.service_uuids();
        if !advertised.is_empty() {
            self.service_uuids = advertised;
            self.sources.set(Feature::ServiceUuids, Datum::Direct);
        }
        if let Some(connectable) = ad.connectable() {
            self.connectable = connectable;
            self.sources.set(Feature::Connectable, Datum::Derived);
        }
        self
    }

    /// The AD type codes the layout feature compares, with the quality of that
    /// evidence.
    ///
    /// Real structures win. With none, the set is synthesized from the fields this
    /// observation carries — which says something about the advertisement's
    /// structure and less than a structure read off the air, hence
    /// [`Datum::Derived`]. An observation with neither answers
    /// [`Datum::Absent`], so the feature contributes neither score nor weight.
    pub fn effective_layout(&self) -> (Vec<u8>, Datum) {
        if !self.field_layout.is_empty() {
            return (
                self.field_layout.clone(),
                self.sources.get(Feature::FieldLayout),
            );
        }
        let synthesized = self.ad_field_types();
        if synthesized.is_empty() {
            (Vec::new(), Datum::Absent)
        } else {
            (synthesized, Datum::Derived)
        }
    }

    /// The payload the similarity feature compares, with the quality of that
    /// evidence.
    ///
    /// Only the manufacturer structure is visible to the stacks in use here, which
    /// is a subset of what "advertisement payload" means in the scoring model; a
    /// feed that only got there through service data reports
    /// [`Datum::Derived`].
    pub fn effective_payload(&self) -> (Vec<u8>, Datum) {
        if !self.manufacturer_data.is_empty() {
            let datum = self.sources.get(Feature::PayloadSimilarity);
            if datum.is_present() {
                return (self.manufacturer_data.clone(), datum);
            }
        }
        let from_service: Vec<u8> = self
            .service_data
            .iter()
            .flat_map(|s| s.data.iter().copied())
            .collect();
        if from_service.is_empty() {
            (Vec::new(), Datum::Absent)
        } else {
            (from_service, Datum::Derived)
        }
    }

    /// Returns the AD field types present in this observation.
    ///
    /// This is a *synthesis* from which fields are populated, used as the fallback
    /// inside [`AdvertisementObservation::effective_layout`] by feeds that never
    /// saw an AD structure. A feed that did see them sets
    /// [`field_layout`](Self::field_layout) and this is not consulted.
    pub fn ad_field_types(&self) -> Vec<u8> {
        let mut types = Vec::new();

        // Flags are almost always present
        if !self.service_uuids.is_empty() || self.manufacturer_id.is_some() {
            types.push(0x01); // Flags
        }

        if self.local_name.is_some() {
            types.extend([0x09, 0x08]); // Complete and Shortened Local Name
        }

        if self.manufacturer_id.is_some() {
            types.push(0xFF); // Manufacturer Specific Data
        }

        if !self.service_uuids.is_empty() {
            types.extend([0x03, 0x07]); // 16-bit and 128-bit Incomplete Service UUIDs
        }

        if !self.service_data.is_empty() {
            types.push(0x16); // Service Data - 16-bit UUID
        }

        if self.tx_power.is_some() {
            types.push(0x0A); // TX Power Level
        }

        if self.appearance.is_some() {
            types.push(0x19); // Appearance
        }

        types.sort();
        types.dedup();
        types
    }
}

/// An opaque logical identity assigned to a group of observations.
///
/// `DeviceIdentity` represents a stable logical identifier that may correspond
/// to one or more physical Bluetooth devices. The identity itself is opaque
/// and does not expose any Bluetooth address information.
///
/// Identities are assigned by an [`IdentityResolver`](crate::IdentityResolver)
/// and remain stable across address rotations when the resolver has sufficient
/// confidence that multiple observations belong to the same physical device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceIdentity(u64);

impl DeviceIdentity {
    /// Creates a new identity from an internal ID.
    pub(crate) fn from_id(id: u64) -> Self {
        Self(id)
    }

    /// Returns the internal ID as a u64.
    pub fn id(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", self.0)
    }
}

/// Historical address record for a physical identity.
#[derive(Debug, Clone)]
#[expect(dead_code)]
pub(crate) struct AddressRecord {
    /// The Bluetooth address.
    pub address: BluetoothAddress,
    /// Address type.
    pub address_type: AddressType,
    /// When this address was first observed.
    pub first_seen: ObservationTime,
    /// When this address was last observed.
    pub last_seen: ObservationTime,
    /// Number of observations with this address.
    pub observation_count: u64,
}

impl AddressRecord {
    pub fn new(
        address: BluetoothAddress,
        address_type: AddressType,
        timestamp: ObservationTime,
    ) -> Self {
        Self {
            address,
            address_type,
            first_seen: timestamp,
            last_seen: timestamp,
            observation_count: 1,
        }
    }

    pub fn update(&mut self, timestamp: ObservationTime) {
        // A replay can hand the same address an older row than the one already
        // recorded; the record keeps the later of the two.
        if timestamp > self.last_seen {
            self.last_seen = timestamp;
        }
        self.observation_count += 1;
    }
}

/// RSSI statistics for a physical identity.
#[derive(Debug, Clone, Default)]
pub(crate) struct RssiStats {
    /// Recent RSSI values for computing rolling statistics.
    pub values: Vec<i16>,
    /// Maximum window size.
    pub max_window: usize,
}

impl RssiStats {
    pub fn new(max_window: usize) -> Self {
        Self {
            values: Vec::with_capacity(max_window),
            max_window,
        }
    }

    pub fn add(&mut self, rssi: i16) {
        self.values.push(rssi);
        if self.values.len() > self.max_window {
            self.values.remove(0);
        }
    }

    /// Returns the rolling average RSSI.
    pub fn average(&self) -> Option<f64> {
        if self.values.is_empty() {
            None
        } else {
            let sum: i32 = self.values.iter().map(|v| *v as i32).sum();
            Some(sum as f64 / self.values.len() as f64)
        }
    }

    /// Returns the RSSI variance.
    #[expect(dead_code)]
    pub fn variance(&self) -> Option<f64> {
        if self.values.len() < 2 {
            None
        } else {
            let avg = self.average()?;
            let sum_sq: f64 = self
                .values
                .iter()
                .map(|v| {
                    let diff = *v as f64 - avg;
                    diff * diff
                })
                .sum();
            Some(sum_sq / self.values.len() as f64)
        }
    }
}

/// A physically inferred device identity with tracking state.
///
/// `PhysicalIdentity` represents the resolver's internal model of a single
/// physical Bluetooth device. It tracks multiple addresses, confidence levels,
/// and statistical features learned from observations over time.
#[derive(Debug, Clone)]
pub(crate) struct PhysicalIdentity {
    /// The logical identity assigned to this physical device.
    pub identity: DeviceIdentity,

    /// Current address record.
    pub current_address: AddressRecord,

    /// Historical addresses that have been associated with this identity.
    pub address_history: Vec<AddressRecord>,

    /// Confidence level (0.0 to 1.0).
    pub confidence: f64,

    /// Confidence history for tracking changes over time.
    pub confidence_history: Vec<(ObservationTime, f64)>,

    /// RSSI statistics.
    pub rssi_stats: RssiStats,

    /// Estimated advertisement interval in milliseconds.
    pub adv_interval_estimate: Option<f64>,

    /// Timestamps of recent observations for interval estimation.
    pub observation_timestamps: Vec<ObservationTime>,

    /// The latest observation seen for this identity.
    pub last_observation: ObservationTime,

    /// Count of total observations for this identity.
    pub observation_count: u64,

    /// Learned stable features (manufacturer ID, service UUIDs, appearance).
    pub stable_features: LearnedFeatures,
}

/// Learned stable features for a physical identity.
///
/// Everything the scorers compare an observation against lives here. The two
/// `Vec<Vec<u8>>` fields hold the most recent observed values rather than a single
/// consensus, because both payloads and AD layouts legitimately vary between
/// advertisements from one device (a rotating counter in a manufacturer payload, a
/// stack that sends the name in one advertisement and not the next) and the scorer
/// wants the best match against recent history, not agreement with a fossil.
#[derive(Debug, Clone, Default)]
pub(crate) struct LearnedFeatures {
    /// Manufacturer ID if consistently observed.
    pub manufacturer_id: Option<u16>,
    /// Service UUIDs consistently observed.
    pub service_uuids: Vec<Uuid>,
    /// Appearance value if consistently observed.
    pub appearance: Option<u16>,
    /// Local name if consistently observed.
    pub local_name: Option<String>,
    /// TX power if consistently observed.
    pub tx_power: Option<i8>,
    /// Whether the device advertises as connectable, when any observation said so.
    pub connectable: Option<bool>,
    /// Recently observed AD type layouts, newest last.
    pub field_layouts: Vec<Vec<u8>>,
    /// Recently observed advertisement payloads, newest last.
    pub payloads: Vec<Vec<u8>>,
    /// How good the evidence behind each learned value is.
    ///
    /// A comparison is only as direct as its weakest side: matching a raw-bytes
    /// observation against a value this identity inferred from a proxy is derived
    /// evidence, not direct evidence, even though one side read the air.
    pub datums: FeatureSources,
}

/// How many recent layouts and payloads an identity remembers.
///
/// Enough to cover a device that alternates between two advertisement forms, small
/// enough that a long-running resolver does not grow with uptime.
pub(crate) const RECENT_FEATURE_WINDOW: usize = 8;

impl PhysicalIdentity {
    /// Creates a new physical identity from an observation.
    pub fn new(
        identity: DeviceIdentity,
        observation: &AdvertisementObservation,
        rssi_window_size: usize,
    ) -> Self {
        let address_record = AddressRecord::new(
            observation.address,
            observation.address_type,
            observation.timestamp,
        );

        PhysicalIdentity {
            identity,
            current_address: address_record,
            address_history: Vec::new(),
            confidence: 0.5, // Start with moderate confidence
            confidence_history: vec![(observation.timestamp, 0.5)],
            // The founding observation counts as the first RSSI sample: an identity
            // that only learned its signal level from the second observation onwards
            // would abstain from the continuity feature exactly once per device.
            rssi_stats: if observation.sources.get(Feature::Rssi).is_present() {
                let mut stats = RssiStats::new(rssi_window_size);
                stats.add(observation.rssi);
                stats
            } else {
                RssiStats::new(rssi_window_size)
            },
            adv_interval_estimate: None,
            observation_timestamps: vec![observation.timestamp],
            last_observation: observation.timestamp,
            observation_count: 1,
            stable_features: LearnedFeatures::from_observation(observation),
        }
    }

    /// Every address this identity answers to: the current one and every address it
    /// has rotated through.
    ///
    /// A device that rotates forward and back is one device, so an identity that
    /// only recognised its newest address would split it in two.
    pub(crate) fn answering_addresses(&self) -> impl Iterator<Item = &BluetoothAddress> {
        std::iter::once(&self.current_address.address)
            .chain(self.address_history.iter().map(|r| &r.address))
    }

    /// Updates this identity with a new observation.
    pub fn update(&mut self, observation: &AdvertisementObservation, _rssi_window_size: usize) {
        // Check if address changed
        if observation.address != self.current_address.address {
            // Move current to history
            let old_record = std::mem::replace(
                &mut self.current_address,
                AddressRecord::new(
                    observation.address,
                    observation.address_type,
                    observation.timestamp,
                ),
            );
            self.address_history.push(old_record);
        } else {
            self.current_address.update(observation.timestamp);
        }

        // Update RSSI stats, but only from an actual measurement: a feed with no
        // signal level puts 0 in the field as a placeholder, and averaging that with
        // real readings would invent a device that is transmitting at 0 dBm.
        if observation.sources.get(Feature::Rssi).is_present() {
            self.rssi_stats.add(observation.rssi);
        }

        // Update advertisement interval estimate
        self.observation_timestamps.push(observation.timestamp);
        if self.observation_timestamps.len() >= 2 {
            let intervals: Vec<f64> = self
                .observation_timestamps
                .windows(2)
                .map(|w| w[1].elapsed_since(&w[0]).as_secs_f64() * 1000.0)
                .collect();

            if !intervals.is_empty() {
                let avg: f64 = intervals.iter().sum::<f64>() / intervals.len() as f64;
                self.adv_interval_estimate = Some(avg);
            }

            // Keep only last 10 timestamps to avoid unbounded growth
            if self.observation_timestamps.len() > 10 {
                self.observation_timestamps.remove(0);
            }
        }

        // A replay does not guarantee arrival order, so the identity's clock is the
        // latest observation, not the most recently processed one.
        if observation.timestamp > self.last_observation {
            self.last_observation = observation.timestamp;
        }
        self.observation_count += 1;

        // Merge stable features
        self.stable_features.merge(observation);
    }

    /// Updates confidence level.
    pub fn update_confidence(&mut self, new_confidence: f64, timestamp: ObservationTime) {
        self.confidence = new_confidence.clamp(0.0, 1.0);
        self.confidence_history.push((timestamp, self.confidence));
    }

    /// Returns whether this identity has expired.
    pub fn is_expired(&self, max_age: std::time::Duration, now: ObservationTime) -> bool {
        now.elapsed_since(&self.last_observation) > max_age
    }
}

impl LearnedFeatures {
    /// Extracts stable features from an observation.
    pub fn from_observation(observation: &AdvertisementObservation) -> Self {
        let (layout, layout_datum) = observation.effective_layout();
        let (payload, payload_datum) = observation.effective_payload();

        let mut datums = FeatureSources::new();
        if observation.manufacturer_id.is_some() {
            datums.set(
                Feature::ManufacturerId,
                observation.sources.get(Feature::ManufacturerId),
            );
        }
        if !observation.service_uuids.is_empty() {
            datums.set(
                Feature::ServiceUuids,
                observation.sources.get(Feature::ServiceUuids),
            );
        }
        if observation.local_name.is_some() {
            datums.set(Feature::Name, observation.sources.get(Feature::Name));
        }
        if observation.appearance.is_some() {
            datums.set(
                Feature::Appearance,
                observation.sources.get(Feature::Appearance),
            );
        }
        if !layout.is_empty() {
            datums.set(Feature::FieldLayout, layout_datum);
        }
        if !payload.is_empty() {
            datums.set(Feature::PayloadSimilarity, payload_datum);
        }
        datums.set(
            Feature::Connectable,
            observation.sources.get(Feature::Connectable),
        );

        Self {
            manufacturer_id: observation.manufacturer_id,
            service_uuids: observation.service_uuids.clone(),
            appearance: observation.appearance,
            local_name: observation.local_name.clone(),
            tx_power: observation.tx_power,
            connectable: observation
                .sources
                .get(Feature::Connectable)
                .is_present()
                .then_some(observation.connectable),
            // Only a layout that was actually read off the air is kept. A layout
            // synthesized from whichever fields this observation happens to carry is
            // a restatement of those fields, and remembering it would let the scorer
            // charge the same evidence twice.
            field_layouts: if layout_datum == Datum::Direct {
                vec![layout]
            } else {
                Vec::new()
            },
            payloads: if payload_datum.is_present() {
                vec![payload]
            } else {
                Vec::new()
            },
            datums,
        }
    }

    /// Merges features from an observation, keeping only consistently observed values.
    ///
    /// A value that agrees with what is already learned keeps the *better* of the two
    /// datums — the same fact seen through a good source is corroborated — and a
    /// value that disagrees is cleared, which is what makes a Direct-quality conflict
    /// visible to the resolver as a contradiction rather than as a permanent
    /// unexplained zero.
    pub fn merge(&mut self, observation: &AdvertisementObservation) {
        // Keep manufacturer_id only if it matches
        if self.manufacturer_id.is_some() && self.manufacturer_id != observation.manufacturer_id {
            self.manufacturer_id = None;
            self.datums.set(Feature::ManufacturerId, Datum::Absent);
        } else if observation.manufacturer_id.is_some() {
            corroborate(
                &mut self.datums,
                Feature::ManufacturerId,
                observation.sources.get(Feature::ManufacturerId),
            );
            if self.manufacturer_id.is_none() {
                self.manufacturer_id = observation.manufacturer_id;
            }
        }

        // Keep only UUIDs that appear consistently
        let mut new_uuids = Vec::new();
        for uuid in &self.service_uuids {
            if observation.service_uuids.contains(uuid) {
                new_uuids.push(*uuid);
            }
        }
        if self.service_uuids.len() != new_uuids.len() {
            // Something the identity learned is no longer advertised.
            self.datums.set(Feature::ServiceUuids, Datum::Absent);
        }
        self.service_uuids = new_uuids;

        // Add new UUIDs if they're in this observation
        for uuid in &observation.service_uuids {
            if !self.service_uuids.contains(uuid) && self.service_uuids.len() < 5 {
                self.service_uuids.push(*uuid);
            }
        }
        if !observation.service_uuids.is_empty() && !self.service_uuids.is_empty() {
            corroborate(
                &mut self.datums,
                Feature::ServiceUuids,
                observation.sources.get(Feature::ServiceUuids),
            );
        }

        // Keep appearance only if consistent
        if self.appearance.is_some() && self.appearance != observation.appearance {
            self.appearance = None;
            self.datums.set(Feature::Appearance, Datum::Absent);
        } else if observation.appearance.is_some() {
            corroborate(
                &mut self.datums,
                Feature::Appearance,
                observation.sources.get(Feature::Appearance),
            );
            if self.appearance.is_none() {
                self.appearance = observation.appearance;
            }
        }

        // A name that changes is a rename, not a different device: the identity keeps
        // the name it learned (scoring the disagreement as evidence against a merge is
        // the scorer's job) and adopts one it never had.
        if self.local_name.is_none() && observation.local_name.is_some() {
            self.local_name = observation.local_name.clone();
            corroborate(
                &mut self.datums,
                Feature::Name,
                observation.sources.get(Feature::Name),
            );
        } else if self.local_name.as_deref() == observation.local_name.as_deref() {
            corroborate(
                &mut self.datums,
                Feature::Name,
                observation.sources.get(Feature::Name),
            );
        }

        if observation.tx_power.is_some() && self.tx_power.is_none() {
            self.tx_power = observation.tx_power;
        }

        if self.connectable.is_none() {
            self.connectable = observation
                .sources
                .get(Feature::Connectable)
                .is_present()
                .then_some(observation.connectable);
        }
        corroborate(
            &mut self.datums,
            Feature::Connectable,
            observation.sources.get(Feature::Connectable),
        );

        let (layout, layout_datum) = observation.effective_layout();
        if layout_datum.is_present() {
            corroborate(&mut self.datums, Feature::FieldLayout, layout_datum);
            // As when the identity was founded: only structures that were really read
            // become part of the forms this identity is known to advertise. The datum
            // still records a guessed layout, so a report can say the feature was
            // estimated rather than seen.
            if layout_datum == Datum::Direct {
                remember_recent(&mut self.field_layouts, layout);
            }
        }

        let (payload, payload_datum) = observation.effective_payload();
        if payload_datum.is_present() {
            remember_recent(&mut self.payloads, payload);
            corroborate(&mut self.datums, Feature::PayloadSimilarity, payload_datum);
        }
    }
}

/// Records that a learned value has been seen through `datum` as well.
///
/// The best source wins: a fact first inferred from a proxy and later read off the
/// air is direct evidence about the value the identity already holds.
fn corroborate(datums: &mut FeatureSources, feature: Feature, datum: Datum) {
    if datum.is_present() {
        datums.set(feature, datums.get(feature).max(datum));
    }
}

/// Records a recent feature value, keeping the window bounded and deduplicated.
///
/// The most recent value moves to the back so that "newest last" stays true, which
/// is what the scorers rely on when they take the tail as the device's current form.
fn remember_recent(recent: &mut Vec<Vec<u8>>, value: Vec<u8>) {
    if value.is_empty() {
        return;
    }
    if let Some(position) = recent.iter().position(|existing| *existing == value) {
        recent.remove(position);
    }
    recent.push(value);
    if recent.len() > RECENT_FEATURE_WINDOW {
        recent.remove(0);
    }
}

/// One feature's contribution to one candidate match.
#[derive(Debug, Clone)]
pub struct FeatureScore {
    /// The feature, by its stable label
    /// ([`Feature::label`](crate::evidence::Feature::label)).
    pub feature: &'static str,
    /// Points earned, after any derived-evidence discount.
    pub score: f64,
    /// Points this feature could have earned — `0.0` when it had nothing to say,
    /// which is what keeps a missing feature out of the denominator.
    pub weight: f64,
    /// Where the compared value came from.
    pub datum: Datum,
}

/// The evidence a merge or a rejection was decided on.
///
/// A resolver decision without its evidence is unreviewable, and these merges are
/// probabilistic inferences about a device that is rotating its address on purpose:
/// the only way to tell a good rule from a lucky one later is to keep the score, the
/// weight that was actually available, and the per-feature breakdown alongside the
/// decision. This is what an offline replay job writes out next to its merges.
#[derive(Debug, Clone)]
pub struct MatchEvidence {
    /// The identity this candidate was scored against.
    pub identity: DeviceIdentity,
    /// Sum of the earned feature scores, in absolute points.
    pub total_score: f64,
    /// Sum of the weights of the features that had something to compare.
    pub available_weight: f64,
    /// Total weight of the designed model, address bypass excluded.
    pub designed_weight: f64,
    /// Whether the candidate was reached by an exact address match, which is
    /// identification rather than inference.
    pub address_matched: bool,
    /// Per-feature breakdown, in the order the scorers ran.
    pub features: Vec<FeatureScore>,
}

impl MatchEvidence {
    /// The fraction of the designed model that could speak to this pair.
    ///
    /// `0.0` when nothing was comparable, which is what stops a feed that sees
    /// nothing from merging everything.
    pub fn coverage(&self) -> f64 {
        if self.designed_weight <= 0.0 {
            0.0
        } else {
            self.available_weight / self.designed_weight
        }
    }

    /// Features that carried the decision, largest contribution first.
    pub fn top_features(&self, limit: usize) -> Vec<(&'static str, f64)> {
        let mut ranked: Vec<(&'static str, f64)> = self
            .features
            .iter()
            .filter(|f| f.score > 0.0)
            .map(|f| (f.feature, f.score))
            .collect();
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(b.0))
        });
        ranked.truncate(limit);
        ranked
    }
}

/// Scoring weights for the matching algorithm.
#[derive(Debug, Clone)]
pub struct ScoringWeights {
    /// Weight for manufacturer ID match (exact match only).
    pub manufacturer_id: f64,
    /// Weight for service UUID overlap.
    pub uuid_overlap: f64,
    /// Weight for appearance match.
    pub appearance: f64,
    /// Weight for AD field layout match.
    pub field_layout: f64,
    /// Weight for payload similarity.
    pub payload_similarity: f64,
    /// Weight for time continuity.
    pub time_continuity: f64,
    /// Weight for RSSI continuity.
    pub rssi: f64,
    /// Weight for local name match.
    pub name: f64,
    /// Weight for connectable flag match.
    pub connectable: f64,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            manufacturer_id: 40.0,
            uuid_overlap: 30.0,
            appearance: 15.0,
            field_layout: 15.0,
            payload_similarity: 20.0,
            time_continuity: 25.0,
            rssi: 10.0,
            name: 25.0,
            connectable: 5.0,
        }
    }
}
