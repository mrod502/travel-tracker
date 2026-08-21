//! Canonical payload structure for cryptographic signing.
//!
//! This module defines the `CanonicalPayload` struct which represents the
//! structured data that will be encoded to CBOR and signed.
//!
//! # Field Order
//!
//! Field order is CRITICAL and MUST match the specification exactly:
//! 0. schema_version
//! 1. signal_type
//! 2. origin_node_id
//! 3. device_hash
//! 4. device_address
//! 5. observed_at_node_local
//! 6. rssi
//! 7. tx_power
//! 8. adv_type
//! 9. location
//! 10. signal_payload
//! 11. advertised_name

/// Canonical payload for cryptographic signing.
///
/// This struct represents the exact data that will be encoded to CBOR
/// and signed by the origin node. Field order is preserved during
/// serialization to ensure deterministic encoding.
///
/// # Specification
///
/// See `.knowledge/implementation/roadmap/phase_0/canonical-payload-spec.md`
/// for the complete field specification and encoding rules.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct CanonicalPayload {
    /// Schema version (MUST be first field for version detection).
    ///
    /// Current version: 1
    pub schema_version: u16,

    /// Type of signal being reported.
    ///
    /// Encoding: u8 with the following values:
    /// - 0 = Bluetooth
    /// - 1 = WiFi
    /// - 2 = NFC
    /// - 3 = Zigbee
    /// - 4 = LoRaWAN
    pub signal_type: u8,

    /// Origin node identity (32-byte SHA-256 hash of signing public key).
    ///
    /// This uniquely identifies the node that captured and signed this observation.
    pub origin_node_id: Vec<u8>,

    /// Device pseudonymous ID (32-byte SHA-256 hash of device address).
    ///
    /// Provides stable pseudonymity while allowing deduplication.
    pub device_hash: Vec<u8>,

    /// Raw MAC/address (6 bytes for BLE/WiFi, variable for others).
    ///
    /// Optional because some signal types may not expose raw addresses.
    pub device_address: Option<Vec<u8>>,

    /// Node-local timestamp in ISO 8601 UTC format.
    ///
    /// This is the raw timestamp BEFORE any clock sync correction.
    /// The sync-corrected timestamp is stored separately in the database.
    pub observed_at_node_local: String,

    /// RSSI value in dBm.
    ///
    /// Signed 16-bit integer to accommodate typical RSSI ranges (-128 to +127).
    pub rssi: i16,

    /// TX power from advertisement payload (if present).
    ///
    /// Optional because not all devices advertise TX power.
    pub tx_power: Option<i16>,

    /// BLE advertisement type (if Bluetooth signal).
    ///
    /// Optional because only applicable to Bluetooth signals.
    pub adv_type: Option<u8>,

    /// Location if from origin node (latitude, longitude as f64).
    ///
    /// Optional because:
    /// - Signal nodes may not have GPS
    /// - Aggregator-added location is NOT covered by signature
    ///
    /// Stored as [lat, lon] in that order.
    pub location: Option<[f64; 2]>,

    /// Raw signal-specific payload data.
    ///
    /// For Bluetooth: JSON-encoded BLE advertisement data
    /// For WiFi: JSON-encoded WiFi scan data
    /// etc.
    ///
    /// Stored as raw bytes to preserve the exact encoding.
    pub signal_payload: Option<Vec<u8>>,

    /// Advertised device name (if present in advertisement).
    ///
    /// Optional because not all devices advertise a name.
    pub advertised_name: Option<String>,
}

impl CanonicalPayload {
    /// Create a new canonical payload builder.
    pub fn builder() -> CanonicalPayloadBuilder {
        CanonicalPayloadBuilder::new()
    }

    /// Get the signal type as a string representation.
    pub fn signal_type_str(&self) -> &'static str {
        match self.signal_type {
            0 => "bluetooth",
            1 => "wifi",
            2 => "nfc",
            3 => "zigbee",
            4 => "lorawan",
            _ => "unknown",
        }
    }
}

/// Builder for creating `CanonicalPayload` instances.
#[derive(Debug)]
pub struct CanonicalPayloadBuilder {
    payload: CanonicalPayload,
}

impl CanonicalPayloadBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            payload: CanonicalPayload {
                schema_version: 1,
                signal_type: 0, // Bluetooth by default
                origin_node_id: Vec::new(),
                device_hash: Vec::new(),
                device_address: None,
                observed_at_node_local: String::new(),
                rssi: 0,
                tx_power: None,
                adv_type: None,
                location: None,
                signal_payload: None,
                advertised_name: None,
            },
        }
    }

    /// Set the schema version.
    pub fn schema_version(mut self, version: u16) -> Self {
        self.payload.schema_version = version;
        self
    }

    /// Set the signal type.
    ///
    /// # Arguments
    /// * `signal_type` - Signal type as u8 (0=bluetooth, 1=wifi, etc.)
    pub fn signal_type(mut self, signal_type: u8) -> Self {
        self.payload.signal_type = signal_type;
        self
    }

    /// Set the signal type from a string.
    ///
    /// # Arguments
    /// * `signal_type` - Signal type as string ("bluetooth", "wifi", etc.)
    ///
    /// # Returns
    /// * `Ok(Self)` if valid signal type
    /// * `Err(String)` if unknown signal type
    pub fn signal_type_str(mut self, signal_type: &str) -> Result<Self, String> {
        let code = match signal_type.to_lowercase().as_str() {
            "bluetooth" => 0,
            "wifi" => 1,
            "nfc" => 2,
            "zigbee" => 3,
            "lorawan" => 4,
            _ => return Err(format!("Unknown signal type: {}", signal_type)),
        };
        self.payload.signal_type = code;
        Ok(self)
    }

    /// Set the origin node ID (32-byte SHA-256 hash).
    pub fn origin_node_id(mut self, node_id: &[u8]) -> Self {
        self.payload.origin_node_id = node_id.to_vec();
        self
    }

    /// Set the device hash (32-byte SHA-256 hash).
    pub fn device_hash(mut self, hash: &[u8]) -> Self {
        self.payload.device_hash = hash.to_vec();
        self
    }

    /// Set the device address.
    pub fn device_address(mut self, address: &[u8]) -> Self {
        self.payload.device_address = Some(address.to_vec());
        self
    }

    /// Clear the device address.
    pub fn no_device_address(mut self) -> Self {
        self.payload.device_address = None;
        self
    }

    /// Set the node-local timestamp (ISO 8601 UTC format).
    pub fn observed_at_node_local(mut self, timestamp: &str) -> Self {
        self.payload.observed_at_node_local = timestamp.to_string();
        self
    }

    /// Set the RSSI value in dBm.
    pub fn rssi(mut self, rssi: i16) -> Self {
        self.payload.rssi = rssi;
        self
    }

    /// Set the TX power.
    pub fn tx_power(mut self, power: i16) -> Self {
        self.payload.tx_power = Some(power);
        self
    }

    /// Clear the TX power.
    pub fn no_tx_power(mut self) -> Self {
        self.payload.tx_power = None;
        self
    }

    /// Set the BLE advertisement type.
    pub fn adv_type(mut self, adv_type: u8) -> Self {
        self.payload.adv_type = Some(adv_type);
        self
    }

    /// Clear the BLE advertisement type.
    pub fn no_adv_type(mut self) -> Self {
        self.payload.adv_type = None;
        self
    }

    /// Set the location (latitude, longitude).
    pub fn location(mut self, lat: f64, lon: f64) -> Self {
        self.payload.location = Some([lat, lon]);
        self
    }

    /// Clear the location.
    pub fn no_location(mut self) -> Self {
        self.payload.location = None;
        self
    }

    /// Set the signal payload.
    pub fn signal_payload(mut self, payload: &[u8]) -> Self {
        self.payload.signal_payload = Some(payload.to_vec());
        self
    }

    /// Clear the signal payload.
    pub fn no_signal_payload(mut self) -> Self {
        self.payload.signal_payload = None;
        self
    }

    /// Set the advertised name.
    pub fn advertised_name(mut self, name: &str) -> Self {
        self.payload.advertised_name = Some(name.to_string());
        self
    }

    /// Clear the advertised name.
    pub fn no_advertised_name(mut self) -> Self {
        self.payload.advertised_name = None;
        self
    }

    /// Build the `CanonicalPayload`.
    pub fn build(self) -> CanonicalPayload {
        self.payload
    }
}

impl Default for CanonicalPayloadBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_basic() {
        let node_id = vec![0u8; 32];
        let device_hash = vec![1u8; 32];
        let device_address = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

        let payload = CanonicalPayload::builder()
            .schema_version(1)
            .signal_type(0)
            .origin_node_id(&node_id)
            .device_hash(&device_hash)
            .device_address(&device_address)
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .tx_power(0)
            .adv_type(0)
            .location(40.6892, -74.0445)
            .signal_payload(b"test payload")
            .advertised_name("TestDevice")
            .build();

        assert_eq!(payload.schema_version, 1);
        assert_eq!(payload.signal_type, 0);
        assert_eq!(payload.origin_node_id, node_id);
        assert_eq!(payload.device_hash, device_hash);
        assert_eq!(payload.device_address, Some(device_address));
        assert_eq!(payload.observed_at_node_local, "2026-08-15T12:00:00Z");
        assert_eq!(payload.rssi, -67);
        assert_eq!(payload.tx_power, Some(0));
        assert_eq!(payload.adv_type, Some(0));
        assert_eq!(payload.location, Some([40.6892, -74.0445]));
        assert_eq!(payload.signal_payload, Some(b"test payload".to_vec()));
        assert_eq!(payload.advertised_name, Some("TestDevice".to_string()));
    }

    #[test]
    fn test_builder_optional_fields() {
        let node_id = vec![0u8; 32];
        let device_hash = vec![1u8; 32];

        let payload = CanonicalPayload::builder()
            .schema_version(1)
            .signal_type(0)
            .origin_node_id(&node_id)
            .device_hash(&device_hash)
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .no_tx_power()
            .no_adv_type()
            .no_location()
            .no_signal_payload()
            .no_advertised_name()
            .build();

        assert!(payload.tx_power.is_none());
        assert!(payload.adv_type.is_none());
        assert!(payload.location.is_none());
        assert!(payload.signal_payload.is_none());
        assert!(payload.advertised_name.is_none());
    }

    #[test]
    fn test_signal_type_str() {
        let payload = CanonicalPayload::builder()
            .signal_type_str("bluetooth").unwrap()
            .build();
        assert_eq!(payload.signal_type_str(), "bluetooth");

        let payload = CanonicalPayload::builder()
            .signal_type_str("wifi").unwrap()
            .build();
        assert_eq!(payload.signal_type_str(), "wifi");

        let payload = CanonicalPayload::builder()
            .signal_type_str("unknown").unwrap_err();
        assert!(payload.contains("Unknown signal type"));
    }

    #[test]
    fn test_builder_minimal() {
        // Test minimal payload with only required fields
        let node_id = vec![0u8; 32];
        let device_hash = vec![1u8; 32];

        let payload = CanonicalPayload::builder()
            .signal_type(0)
            .origin_node_id(&node_id)
            .device_hash(&device_hash)
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .build();

        assert_eq!(payload.schema_version, 1); // Default
        assert!(payload.device_address.is_none());
        assert!(payload.tx_power.is_none());
    }
}
