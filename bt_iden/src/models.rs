//! Core data models for Bluetooth LE advertisement observations and identity tracking.
//!
//! This module defines the fundamental data structures used throughout the identity
//! resolution engine, including advertisement observations, device identities, and
//! physical identity tracking.

use std::time::Instant;
use uuid::Uuid;

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
/// observation was obtained (BlueZ, raw HCI, pcap, etc.).
///
/// # Example
///
/// ```
/// use std::time::Instant;
/// use bt_iden::models::{AdvertisementObservation, BluetoothAddress, AddressType};
///
/// let observation = AdvertisementObservation {
///     timestamp: Instant::now(),
///     address: BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
///     address_type: AddressType::PrivateResolvable,
///     rssi: -65,
///     connectable: true,
///     manufacturer_id: Some(0x004C), // Apple
///     manufacturer_data: vec![0x01, 0x02, 0x03],
///     service_data: vec![],
///     service_uuids: vec![],
///     local_name: None,
///     tx_power: None,
///     appearance: None,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct AdvertisementObservation {
    /// When the observation was captured.
    pub timestamp: Instant,

    /// The Bluetooth source address.
    pub address: BluetoothAddress,

    /// The type of address (public, random, etc.).
    pub address_type: AddressType,

    /// Received Signal Strength Indicator in dBm.
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
}

impl AdvertisementObservation {
    /// Creates a new observation with the given timestamp and address.
    pub fn new(timestamp: Instant, address: BluetoothAddress, address_type: AddressType) -> Self {
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
        }
    }

    /// Sets the RSSI value.
    pub fn with_rssi(mut self, rssi: i16) -> Self {
        self.rssi = rssi;
        self
    }

    /// Sets the connectable flag.
    pub fn with_connectable(mut self, connectable: bool) -> Self {
        self.connectable = connectable;
        self
    }

    /// Sets manufacturer-specific data.
    pub fn with_manufacturer_data(mut self, id: u16, data: Vec<u8>) -> Self {
        self.manufacturer_id = Some(id);
        self.manufacturer_data = data;
        self
    }

    /// Adds a service UUID.
    pub fn with_service_uuid(mut self, uuid: Uuid) -> Self {
        self.service_uuids.push(uuid);
        self
    }

    /// Adds service data.
    pub fn with_service_data(mut self, uuid: Uuid, data: Vec<u8>) -> Self {
        self.service_data.push(ServiceData { uuid, data });
        self
    }

    /// Sets the local name.
    pub fn with_local_name(mut self, name: String) -> Self {
        self.local_name = Some(name);
        self
    }

    /// Sets the TX power level.
    pub fn with_tx_power(mut self, tx_power: i8) -> Self {
        self.tx_power = Some(tx_power);
        self
    }

    /// Sets the appearance value.
    pub fn with_appearance(mut self, appearance: u16) -> Self {
        self.appearance = Some(appearance);
        self
    }

    /// Returns the AD field types present in this observation.
    ///
    /// This is used for comparing advertisement structure between observations.
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
    pub first_seen: Instant,
    /// When this address was last observed.
    pub last_seen: Instant,
    /// Number of observations with this address.
    pub observation_count: u64,
}

impl AddressRecord {
    pub fn new(address: BluetoothAddress, address_type: AddressType, timestamp: Instant) -> Self {
        Self {
            address,
            address_type,
            first_seen: timestamp,
            last_seen: timestamp,
            observation_count: 1,
        }
    }

    pub fn update(&mut self, timestamp: Instant) {
        self.last_seen = timestamp;
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
    pub confidence_history: Vec<(Instant, f64)>,

    /// RSSI statistics.
    pub rssi_stats: RssiStats,

    /// Estimated advertisement interval in milliseconds.
    pub adv_interval_estimate: Option<f64>,

    /// Timestamps of recent observations for interval estimation.
    pub observation_timestamps: Vec<Instant>,

    /// Maximum timestamp seen for this identity.
    pub last_observation: Instant,

    /// Count of total observations for this identity.
    pub observation_count: u64,

    /// Learned stable features (manufacturer ID, service UUIDs, appearance).
    pub stable_features: LearnedFeatures,
}

/// Learned stable features for a physical identity.
#[derive(Debug, Clone, Default)]
#[expect(dead_code)]
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
}

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
            rssi_stats: RssiStats::new(rssi_window_size),
            adv_interval_estimate: None,
            observation_timestamps: vec![observation.timestamp],
            last_observation: observation.timestamp,
            observation_count: 1,
            stable_features: LearnedFeatures::from_observation(observation),
        }
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

        // Update RSSI stats
        self.rssi_stats.add(observation.rssi);

        // Update advertisement interval estimate
        self.observation_timestamps.push(observation.timestamp);
        if self.observation_timestamps.len() >= 2 {
            let intervals: Vec<f64> = self
                .observation_timestamps
                .windows(2)
                .map(|w| w[1].duration_since(w[0]).as_secs_f64() * 1000.0)
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

        self.last_observation = observation.timestamp;
        self.observation_count += 1;

        // Merge stable features
        self.stable_features.merge(observation);
    }

    /// Updates confidence level.
    pub fn update_confidence(&mut self, new_confidence: f64, timestamp: Instant) {
        self.confidence = new_confidence.clamp(0.0, 1.0);
        self.confidence_history.push((timestamp, self.confidence));
    }

    /// Returns whether this identity has expired.
    pub fn is_expired(&self, max_age: std::time::Duration, now: Instant) -> bool {
        now.duration_since(self.last_observation) > max_age
    }
}

impl LearnedFeatures {
    /// Extracts stable features from an observation.
    pub fn from_observation(observation: &AdvertisementObservation) -> Self {
        Self {
            manufacturer_id: observation.manufacturer_id,
            service_uuids: observation.service_uuids.clone(),
            appearance: observation.appearance,
            local_name: observation.local_name.clone(),
            tx_power: observation.tx_power,
        }
    }

    /// Merges features from an observation, keeping only consistently observed values.
    pub fn merge(&mut self, observation: &AdvertisementObservation) {
        // Keep manufacturer_id only if it matches
        if self.manufacturer_id.is_some() && self.manufacturer_id != observation.manufacturer_id {
            self.manufacturer_id = None;
        } else if observation.manufacturer_id.is_some() && self.manufacturer_id.is_none() {
            self.manufacturer_id = observation.manufacturer_id;
        }

        // Keep only UUIDs that appear consistently
        let mut new_uuids = Vec::new();
        for uuid in &self.service_uuids {
            if observation.service_uuids.contains(uuid) {
                new_uuids.push(*uuid);
            }
        }
        self.service_uuids = new_uuids;

        // Add new UUIDs if they're in this observation
        for uuid in &observation.service_uuids {
            if !self.service_uuids.contains(uuid) && self.service_uuids.len() < 5 {
                self.service_uuids.push(*uuid);
            }
        }

        // Keep appearance only if consistent
        if self.appearance.is_some() && self.appearance != observation.appearance {
            self.appearance = None;
        } else if observation.appearance.is_some() && self.appearance.is_none() {
            self.appearance = observation.appearance;
        }
    }
}

/// A candidate match between an observation and a physical identity.
#[derive(Debug, Clone)]
pub(crate) struct MatchCandidate {
    /// The identity being considered.
    pub identity: DeviceIdentity,
    /// Total match score.
    pub total_score: f64,
    /// Individual component scores for debugging.
    pub component_scores: std::collections::HashMap<String, f64>,
}

impl MatchCandidate {
    pub fn new(identity: DeviceIdentity, total_score: f64) -> Self {
        let mut component_scores = std::collections::HashMap::new();
        component_scores.insert("total".to_string(), total_score);
        Self {
            identity,
            total_score,
            component_scores,
        }
    }

    pub fn with_component(mut self, name: String, score: f64) -> Self {
        self.component_scores.insert(name, score);
        self
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
