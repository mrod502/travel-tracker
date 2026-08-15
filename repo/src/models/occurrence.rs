use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::PostgisPoint;

use super::enums::{AdvType, BleAddressType, LocationSource};

// ============================================================================
// COMMON OCCURRENCE TYPES (signal-agnostic)
// ============================================================================

/// Type of wireless signal
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "signal_type", rename_all = "snake_case")]
pub enum SignalType {
    Bluetooth,
    Wifi,
    Nfc,
    Zigbee,
    Lorawan,
}

impl SignalType {
    /// Returns all possible signal types
    pub fn all() -> &'static [SignalType] {
        &[SignalType::Bluetooth, SignalType::Wifi, SignalType::Nfc, SignalType::Zigbee, SignalType::Lorawan]
    }
}

/// Represents a wireless signal occurrence (Bluetooth, WiFi, etc.)
///
/// This is an append-only record of a wireless signal being observed by a node.
/// The table does not support updates - each occurrence is a unique event.
///
/// NOTE: This model uses the unified `occurrences` table that supports multiple
/// signal types via the `signal_type` discriminator and `signal_payload` JSONB column.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Occurrence {
    /// UUIDv7 - time-sortable unique identifier for this occurrence
    pub occurrence_id: Uuid,

    /// Type of wireless signal (bluetooth, wifi, nfc, etc.)
    pub signal_type: SignalType,

    /// ID of the node that CAPTURED and SIGNED this observation
    /// Node identity is SHA-256(signing_public_key) - 32 bytes
    pub origin_node_id: Vec<u8>,

    /// UTC timestamp after clock sync correction
    pub observed_at: DateTime<Utc>,

    /// Raw node-local timestamp before sync correction (for drift auditing)
    pub observed_at_node_local: DateTime<Utc>,

    // --- Device Information (common across signal types) ---
    /// Raw MAC/address (6 bytes for BLE/WiFi, variable for others)
    pub device_address: Option<Vec<u8>>,

    /// Stable pseudonymous ID (SHA-256 hash - 32 bytes)
    pub device_hash: Vec<u8>,

    /// Device name (if present)
    pub advertised_name: Option<String>,

    // --- Signal-Specific Data (consolidated in signal_payload) ---
    /// BLE-specific: advertisement type
    pub adv_type: Option<AdvType>,

    /// Signal strength in dBm
    pub rssi: i16,

    /// TX power (if present)
    pub tx_power: Option<i16>,

    /// Signal-specific payload (JSONB)
    /// Bluetooth: { ble: { service_uuids, manufacturer_data, raw_payload_hex, address_type, ... } }
    /// WiFi: { wifi: { ssid, bssid, channel, capabilities, ... } }
    pub signal_payload: serde_json::Value,

    // --- Location Data ---
    /// Location as PostGIS Geography type (serialized via sqlx)
    /// Use PostgisPoint wrapper for proper handling
    pub location: Option<PostgisPoint>,

    /// Altitude in meters
    pub alt_m: Option<f32>,

    /// GPS accuracy in meters
    pub accuracy_m: Option<f32>,

    /// Location source
    pub location_source: LocationSource,

    // --- Generated H3 Geo-Cells (from location) ---
    /// Fine-grained H3 cell (Resolution 9, ~0.1 km²)
    /// Note: These are generated columns in the database, not explicitly inserted
    pub geo_cell_fine: Option<i64>,

    /// Macro H3 cell (Resolution 6, ~36 km²)
    /// Note: These are generated columns in the database, not explicitly inserted
    pub geo_cell_macro: Option<i64>,

    // --- Provenance ---
    /// Canonical bytes that were signed (see canonical-cbor-spec.md)
    pub signed_payload: Vec<u8>,

    /// Ed25519 signature (64 bytes)
    pub signature: Vec<u8>,

    // --- Metadata ---
    /// Schema version for forward compatibility
    pub schema_version: i16,

    /// Database insertion timestamp
    pub ingested_at: DateTime<Utc>,
}

impl Occurrence {
    /// Create a new occurrence using the builder pattern
    pub fn builder() -> OccurrenceBuilder {
        OccurrenceBuilder::new()
    }
}

// ============================================================================
// OCCURRENCE BUILDER
// ============================================================================

/// Builder for creating `Occurrence` instances with a fluent API
///
/// # Example
///
/// ```ignore
/// let occurrence = Occurrence::builder()
///     .signal_type(SignalType::Bluetooth)
///     .origin_node_id(&node_id)
///     .observed_at(Utc::now())
///     .observed_at_node_local(Utc::now())
///     .device_hash(&device_hash)
///     .rssi(-67)
///     .signal_payload(serde_json::json!({}))
///     .signed_payload(&signed_payload)
///     .signature(&signature)
///     .device_address(&mac)
///     .adv_type(AdvType::ConnectableAdv)
///     .tx_power(0)
///     .advertised_name("MyDevice")
///     .with_location(40.6892, -74.0445, Some(10.0), Some(5.0), LocationSource::NodeGps)
///     .build();
/// ```
pub struct OccurrenceBuilder {
    inner: Occurrence,
}

impl OccurrenceBuilder {
    /// Create a new builder with sensible defaults
    pub fn new() -> Self {
        Self {
            inner: Occurrence {
                occurrence_id: Uuid::now_v7(),
                observed_at_node_local: Utc::now(),
                schema_version: 1,
                ingested_at: Utc::now(),
                ..Default::default()
            },
        }
    }

    // ==================== Required Fields ====================

    /// Set the signal type (bluetooth, wifi, nfc, etc.)
    pub fn signal_type(mut self, signal_type: SignalType) -> Self {
        self.inner.signal_type = signal_type;
        self
    }

    /// Set the origin node ID (32-byte SHA-256 hash)
    pub fn origin_node_id(mut self, id: &[u8]) -> Self {
        self.inner.origin_node_id = id.to_vec();
        self
    }

    /// Set the UTC timestamp after clock sync correction
    pub fn observed_at(mut self, observed_at: DateTime<Utc>) -> Self {
        self.inner.observed_at = observed_at;
        self
    }

    /// Set the raw node-local timestamp before sync correction
    pub fn observed_at_node_local(mut self, observed_at_node_local: DateTime<Utc>) -> Self {
        self.inner.observed_at_node_local = observed_at_node_local;
        self
    }

    /// Set the device hash (32-byte SHA-256 hash)
    pub fn device_hash(mut self, hash: &[u8]) -> Self {
        self.inner.device_hash = hash.to_vec();
        self
    }

    /// Set the signal strength in dBm
    pub fn rssi(mut self, rssi: i16) -> Self {
        self.inner.rssi = rssi;
        self
    }

    /// Set the signal-specific payload (JSONB)
    pub fn signal_payload(mut self, payload: serde_json::Value) -> Self {
        self.inner.signal_payload = payload;
        self
    }

    /// Set the canonical signed payload bytes
    pub fn signed_payload(mut self, payload: &[u8]) -> Self {
        self.inner.signed_payload = payload.to_vec();
        self
    }

    /// Set the Ed25519 signature (64 bytes)
    pub fn signature(mut self, signature: &[u8]) -> Self {
        self.inner.signature = signature.to_vec();
        self
    }

    // ==================== Optional Fields ====================

    /// Set the device MAC/address
    pub fn device_address(mut self, address: &[u8]) -> Self {
        self.inner.device_address = Some(address.to_vec());
        self
    }

    /// Set the device advertised name
    pub fn advertised_name(mut self, name: &str) -> Self {
        self.inner.advertised_name = Some(name.to_string());
        self
    }

    /// Set the BLE advertisement type
    pub fn adv_type(mut self, adv_type: AdvType) -> Self {
        self.inner.adv_type = Some(adv_type);
        self
    }

    /// Set the TX power
    pub fn tx_power(mut self, power: i16) -> Self {
        self.inner.tx_power = Some(power);
        self
    }

    // ==================== Convenience Methods ====================

    /// Set location data
    pub fn with_location(
        mut self,
        lat: f64,
        lon: f64,
        alt_m: Option<f32>,
        accuracy_m: Option<f32>,
        source: LocationSource,
    ) -> Self {
        self.inner.location = Some(PostgisPoint(geo_types::Point::<f64>::new(lon, lat)));
        self.inner.alt_m = alt_m;
        self.inner.accuracy_m = accuracy_m;
        self.inner.location_source = source;
        self
    }

    /// Set BLE-specific data
    ///
    /// Convenience method that sets device_address, adv_type, tx_power,
    /// advertised_name, and builds the BLE signal_payload in one call.
    #[allow(clippy::too_many_arguments)]
    pub fn with_bluetooth_data(
        mut self,
        address: &[u8],
        address_type: BleAddressType,
        adv_type: AdvType,
        advertised_name: Option<&str>,
        tx_power: Option<i16>,
        service_uuids: Option<Vec<Vec<u8>>>,
        manufacturer_data: Option<&[u8]>,
        raw_payload_hex: &str,
    ) -> Self {
        self.inner.device_address = Some(address.to_vec());
        self.inner.adv_type = Some(adv_type);

        if let Some(name) = advertised_name {
            self.inner.advertised_name = Some(name.to_string());
        }

        if let Some(power) = tx_power {
            self.inner.tx_power = Some(power);
        }

        // Build BLE-specific payload
        let mut ble_payload = serde_json::Map::new();
        ble_payload.insert(
            "address_type".to_string(),
            serde_json::json!(format!("{:?}", address_type).to_lowercase()),
        );
        ble_payload.insert(
            "adv_type".to_string(),
            serde_json::json!(format!("{:?}", adv_type).to_lowercase()),
        );

        if let Some(uuids) = service_uuids {
            ble_payload.insert("service_uuids".to_string(), serde_json::json!(uuids));
        }
        if let Some(data) = manufacturer_data {
            ble_payload.insert("manufacturer_data".to_string(), serde_json::json!(data));
        }
        ble_payload.insert("raw_payload_hex".to_string(), serde_json::json!(raw_payload_hex));

        // Wrap in signal_type key
        let mut payload = serde_json::Map::new();
        payload.insert("ble".to_string(), serde_json::json!(ble_payload));

        self.signal_payload(serde_json::json!(payload))
    }

    /// Build the `Occurrence`
    pub fn build(self) -> Occurrence {
        self.inner
    }
}

impl Default for OccurrenceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// OCCURRENCE RELAY MODEL
// ============================================================================

/// Tracks which node relayed an occurrence on behalf of the origin node
///
/// Separated from occurrences to distinguish:
/// 1. What was observed (occurrences - the core data)
/// 2. Who reported it on behalf of whom (occurrence_relays - relay metadata)
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct OccurrenceRelay {
    /// Reference to the occurrence being relayed
    pub occurrence_id: Uuid,

    /// Must match occurrence timestamp
    pub observed_at: DateTime<Utc>,

    /// Must match occurrence geo_cell_macro
    pub geo_cell_macro: u64,

    /// Node that wrote this relay record (32-byte SHA-256 hash)
    pub reporting_node_id: Vec<u8>,

    /// When this relay was recorded
    pub ingested_at: DateTime<Utc>,
}

impl OccurrenceRelay {
    /// Create a new occurrence relay record
    pub fn new(
        occurrence_id: Uuid,
        observed_at: DateTime<Utc>,
        geo_cell_macro: u64,
        reporting_node_id: &[u8],
    ) -> Self {
        Self {
            occurrence_id,
            observed_at,
            geo_cell_macro,
            reporting_node_id: reporting_node_id.to_vec(),
            ingested_at: Utc::now(),
        }
    }
}

// ============================================================================
// DEFAULT IMPLEMENTATIONS
// ============================================================================

impl Default for Occurrence {
    fn default() -> Self {
        Self {
            occurrence_id: Uuid::nil(),
            signal_type: SignalType::Bluetooth,
            origin_node_id: Vec::new(),
            observed_at: Utc::now(),
            observed_at_node_local: Utc::now(),
            device_address: None,
            device_hash: Vec::new(),
            advertised_name: None,
            adv_type: None,
            rssi: 0,
            tx_power: None,
            signal_payload: serde_json::json!({}),
            location: None,
            alt_m: None,
            accuracy_m: None,
            location_source: LocationSource::NodeGps,
            geo_cell_fine: None,
            geo_cell_macro: None,
            signed_payload: Vec::new(),
            signature: Vec::new(),
            schema_version: 1,
            ingested_at: Utc::now(),
        }
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_occurrence_builder_basic() {
        let node_id = vec![0u8; 32]; // 32-byte SHA-256 hash
        let device_hash = vec![1u8; 32]; // 32-byte SHA-256 hash
        let signed_payload = vec![2u8; 32];
        let signature = vec![3u8; 64];

        let occurrence = Occurrence::builder()
            .signal_type(SignalType::Bluetooth)
            .origin_node_id(&node_id)
            .observed_at(Utc::now())
            .observed_at_node_local(Utc::now())
            .device_hash(&device_hash)
            .rssi(-67)
            .signal_payload(serde_json::json!({}))
            .signed_payload(&signed_payload)
            .signature(&signature)
            .build();

        assert_eq!(occurrence.signal_type, SignalType::Bluetooth);
        assert_eq!(occurrence.origin_node_id, node_id);
        assert_eq!(occurrence.device_hash, device_hash);
        assert_eq!(occurrence.rssi, -67);
        assert_eq!(occurrence.schema_version, 1);
    }

    #[test]
    fn test_bluetooth_payload() {
        let node_id = vec![0u8; 32];
        let device_hash = vec![1u8; 32];
        let mac = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let signed_payload = vec![2u8; 32];
        let signature = vec![3u8; 64];

        let occurrence = Occurrence::builder()
            .signal_type(SignalType::Bluetooth)
            .origin_node_id(&node_id)
            .observed_at(Utc::now())
            .observed_at_node_local(Utc::now())
            .device_hash(&device_hash)
            .rssi(-67)
            .signal_payload(serde_json::json!({}))
            .signed_payload(&signed_payload)
            .signature(&signature)
            .with_bluetooth_data(
                &mac,
                BleAddressType::Public,
                AdvType::ConnectableAdv,
                Some("TestDevice"),
                Some(0),
                Some(vec![vec![0x18, 0x0D]]),
                Some(&[0xFF, 0x00]),
                "020106",
            )
            .build();

        assert_eq!(occurrence.device_address, Some(mac));
        assert_eq!(occurrence.adv_type, Some(AdvType::ConnectableAdv));
        assert_eq!(occurrence.advertised_name, Some("TestDevice".to_string()));

        let payload = occurrence.signal_payload.as_object().unwrap();
        assert!(payload.contains_key("ble"));
    }

    #[test]
    fn test_occurrence_with_location() {
        let node_id = vec![0u8; 32];
        let device_hash = vec![1u8; 32];
        let signed_payload = vec![2u8; 32];
        let signature = vec![3u8; 64];

        let occurrence = Occurrence::builder()
            .signal_type(SignalType::Bluetooth)
            .origin_node_id(&node_id)
            .observed_at(Utc::now())
            .observed_at_node_local(Utc::now())
            .device_hash(&device_hash)
            .rssi(-67)
            .signal_payload(serde_json::json!({}))
            .signed_payload(&signed_payload)
            .signature(&signature)
            .with_location(40.6892, -74.0445, Some(10.0), Some(5.0), LocationSource::NodeGps)
            .build();

        assert!(occurrence.location.is_some());
        let location = occurrence.location.unwrap();
        assert!((location.0.y() - 40.6892).abs() < f64::EPSILON);
        assert!((location.0.x() - (-74.0445)).abs() < f64::EPSILON);
        assert_eq!(occurrence.alt_m, Some(10.0));
        assert_eq!(occurrence.accuracy_m, Some(5.0));
    }

    #[test]
    fn test_occurrence_relay_new() {
        let occurrence_id = Uuid::now_v7();
        let node_id = vec![0u8; 32];

        let relay = OccurrenceRelay::new(
            occurrence_id,
            Utc::now(),
            0x8a2a100000000000, // Example H3 cell
            &node_id,
        );

        assert_eq!(relay.occurrence_id, occurrence_id);
        assert_eq!(relay.reporting_node_id, node_id);
        assert_eq!(relay.geo_cell_macro, 0x8a2a100000000000);
    }

    #[test]
    fn test_builder_individual_fields() {
        let node_id = vec![0u8; 32];
        let device_hash = vec![1u8; 32];
        let mac = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let signed_payload = vec![2u8; 32];
        let signature = vec![3u8; 64];

        let occurrence = Occurrence::builder()
            .signal_type(SignalType::Wifi)
            .origin_node_id(&node_id)
            .observed_at(Utc::now())
            .observed_at_node_local(Utc::now())
            .device_hash(&device_hash)
            .rssi(-72)
            .signal_payload(serde_json::json!({"wifi": {}}))
            .signed_payload(&signed_payload)
            .signature(&signature)
            .device_address(&mac)
            .advertised_name("MyNetwork")
            .tx_power(-20)
            .build();

        assert_eq!(occurrence.signal_type, SignalType::Wifi);
        assert_eq!(occurrence.device_address, Some(mac));
        assert_eq!(occurrence.advertised_name, Some("MyNetwork".to_string()));
        assert_eq!(occurrence.tx_power, Some(-20));
    }
}
