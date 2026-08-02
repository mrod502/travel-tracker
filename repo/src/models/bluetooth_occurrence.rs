use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Represents a single Bluetooth device advertisement occurrence
///
/// This is an append-only record of a Bluetooth device being observed by a node.
/// The table does not support updates - each occurrence is a unique event.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct BluetoothOccurrence {
    /// UUIDv7 - time-sortable unique identifier for this occurrence
    pub id: Uuid,

    /// ID of the node that observed this device
    /// Node identity is tied to a CA-issued certificate
    /// (certificate data stored separately for verification)
    pub node_id: Uuid,

    /// UTC timestamp after clock sync correction
    pub observed_at: DateTime<Utc>,

    /// Raw node-local timestamp before sync correction (for drift auditing)
    pub observed_at_node_local: Option<DateTime<Utc>>,

    // --- Device Information ---
    /// BLE MAC address (6 bytes raw)
    /// May be randomized/rotating for privacy
    pub device_address: Vec<u8>,

    /// Address type: public, random_static, random_resolvable, random_nonresolvable
    pub device_address_type: Option<String>,

    /// Device name from AD payload (if present)
    pub device_advertised_name: Option<String>,

    /// Stable pseudonymous ID for privacy (SHA256 hash of address - 32 bytes)
    pub device_hash: Option<Vec<u8>>,

    // --- Advertisement Details ---
    /// Advertisement type: ADV_IND, ADV_NONCONN_IND, SCAN_RSP, etc.
    pub advertisement_type: Option<String>,

    /// Signal strength in dBm
    pub rssi: Option<i32>,

    /// TX power from AD payload (if present)
    pub tx_power: Option<i32>,

    /// Advertised service UUIDs (array of 16-byte UUIDs)
    pub service_uuids: Option<Vec<Vec<u8>>>,

    /// Bluetooth SIG Company ID (16-bit, stored as i32)
    pub manufacturer_company_id: Option<i32>,

    /// Raw manufacturer data payload (up to 28 bytes)
    pub manufacturer_payload: Option<Vec<u8>>,

    /// Full raw advertisement payload for reprocessing
    pub raw_payload: Option<Vec<u8>>,

    // --- Location Data ---
    /// Latitude (NUMERIC(10,7) - ~1cm accuracy)
    pub location_lat: Option<BigDecimal>,

    /// Longitude (NUMERIC(11,7) - ~1cm accuracy)
    pub location_lon: Option<BigDecimal>,

    /// Altitude in meters (NUMERIC(8,3) - 1mm precision)
    pub location_alt_m: Option<BigDecimal>,

    /// GPS fix accuracy in meters (NUMERIC(6,3))
    pub location_accuracy_m: Option<BigDecimal>,

    /// Location source: node_fixed, node_gps, interpolated
    pub location_source: Option<String>,

    // --- Metadata ---
    /// Schema version for forward compatibility
    pub schema_version: i32,

    /// Database insertion timestamp
    pub created_at: DateTime<Utc>,
}

impl BluetoothOccurrence {
    /// Create a new Bluetooth occurrence
    ///
    /// # Arguments
    /// * `id` - UUIDv7 identifier (use `Uuid::now_v7()`)
    /// * `node_id` - UUID of the observing node (from certificate)
    /// * `observed_at` - UTC timestamp after clock sync
    /// * `device_address` - BLE MAC address as 6-byte array
    ///
    /// # Returns
    /// A new `BluetoothOccurrence` with default values for optional fields
    pub fn new(id: Uuid, node_id: Uuid, observed_at: DateTime<Utc>, device_address: &[u8]) -> Self {
        Self {
            id,
            node_id,
            observed_at,
            observed_at_node_local: None,
            device_address: device_address.to_vec(),
            device_address_type: None,
            device_advertised_name: None,
            device_hash: None,
            advertisement_type: None,
            rssi: None,
            tx_power: None,
            service_uuids: None,
            manufacturer_company_id: None,
            manufacturer_payload: None,
            raw_payload: None,
            location_lat: None,
            location_lon: None,
            location_alt_m: None,
            location_accuracy_m: None,
            location_source: None,
            schema_version: 1,
            created_at: Utc::now(),
        }
    }

    /// Set the raw node-local timestamp
    pub fn with_node_local_timestamp(mut self, ts: DateTime<Utc>) -> Self {
        self.observed_at_node_local = Some(ts);
        self
    }

    /// Set the device address type
    pub fn with_address_type(mut self, address_type: impl Into<String>) -> Self {
        self.device_address_type = Some(address_type.into());
        self
    }

    /// Set the advertised name
    pub fn with_advertised_name(mut self, name: impl Into<String>) -> Self {
        self.device_advertised_name = Some(name.into());
        self
    }

    /// Set the device hash for privacy (SHA256 - 32 bytes)
    pub fn with_device_hash(mut self, hash: &[u8]) -> Self {
        self.device_hash = Some(hash.to_vec());
        self
    }

    /// Set advertisement type
    pub fn with_advertisement_type(mut self, adv_type: impl Into<String>) -> Self {
        self.advertisement_type = Some(adv_type.into());
        self
    }

    /// Set RSSI
    pub fn with_rssi(mut self, rssi: i32) -> Self {
        self.rssi = Some(rssi);
        self
    }

    /// Set TX power
    pub fn with_tx_power(mut self, tx_power: i32) -> Self {
        self.tx_power = Some(tx_power);
        self
    }

    /// Set service UUIDs (array of 16-byte UUIDs)
    pub fn with_service_uuids(mut self, uuids: Vec<Vec<u8>>) -> Self {
        self.service_uuids = Some(uuids);
        self
    }

    /// Set manufacturer data
    ///
    /// # Arguments
    /// * `company_id` - Bluetooth SIG Company ID (16-bit value)
    /// * `payload` - Raw manufacturer data (up to 28 bytes)
    pub fn with_manufacturer_data(mut self, company_id: i32, payload: &[u8]) -> Self {
        self.manufacturer_company_id = Some(company_id);
        self.manufacturer_payload = Some(payload.to_vec());
        self
    }

    /// Set raw payload
    pub fn with_raw_payload(mut self, payload: &[u8]) -> Self {
        self.raw_payload = Some(payload.to_vec());
        self
    }

    /// Set location coordinates
    pub fn with_location(
        mut self,
        lat: BigDecimal,
        lon: BigDecimal,
        alt_m: Option<BigDecimal>,
        accuracy_m: Option<BigDecimal>,
        source: impl Into<String>,
    ) -> Self {
        self.location_lat = Some(lat);
        self.location_lon = Some(lon);
        self.location_alt_m = alt_m;
        self.location_accuracy_m = accuracy_m;
        self.location_source = Some(source.into());
        self
    }

    /// Set schema version
    pub fn with_schema_version(mut self, version: i32) -> Self {
        self.schema_version = version;
        self
    }
}

/// Helper function to convert a 6-byte MAC address to a display string
pub fn mac_address_to_string(mac: &[u8]) -> String {
    if mac.len() != 6 {
        return format!("<invalid MAC: {} bytes>", mac.len());
    }
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Helper function to parse a MAC address string to bytes
pub fn mac_address_from_string(s: &str) -> Result<Vec<u8>, &'static str> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return Err("MAC address must have 6 octets separated by ':'");
    }

    let mut result = Vec::with_capacity(6);
    for part in parts {
        if part.len() != 2 {
            return Err("Each octet must be exactly 2 hex digits");
        }
        match u8::from_str_radix(part, 16) {
            Ok(val) => result.push(val),
            Err(_) => return Err("Invalid hex digit in MAC address"),
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_occurrence() {
        let id = Uuid::now_v7();
        let node_id = Uuid::now_v7();
        let mac = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

        let occurrence = BluetoothOccurrence::new(id, node_id, Utc::now(), &mac);

        assert_eq!(occurrence.id, id);
        assert_eq!(occurrence.node_id, node_id);
        assert_eq!(occurrence.device_address, mac);
        assert_eq!(occurrence.schema_version, 1);
        assert!(occurrence.observed_at_node_local.is_none());
    }

    #[test]
    fn test_builder_pattern() {
        let id = Uuid::now_v7();
        let node_id = Uuid::now_v7();
        let mac = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

        let occurrence = BluetoothOccurrence::new(id, node_id, Utc::now(), &mac)
            .with_rssi(-67)
            .with_advertisement_type("adv_ind")
            .with_advertised_name("Device-1234");

        assert_eq!(occurrence.rssi, Some(-67));
        assert_eq!(occurrence.advertisement_type, Some("adv_ind".to_string()));
        assert_eq!(
            occurrence.device_advertised_name,
            Some("Device-1234".to_string())
        );
    }

    #[test]
    fn test_mac_address_conversion() {
        let mac_str = "AA:BB:CC:DD:EE:FF";
        let mac_bytes = mac_address_from_string(mac_str).unwrap();
        assert_eq!(mac_bytes, vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);

        let converted_back = mac_address_to_string(&mac_bytes);
        assert_eq!(converted_back, mac_str);
    }

    #[test]
    fn test_mac_address_invalid() {
        assert!(mac_address_from_string("AA:BB:CC:DD:EE").is_err());
        assert!(mac_address_from_string("AA:BB:CC:DD:EE:FF:GG").is_err());
        assert!(mac_address_from_string("AABB:CCDD:EEFF").is_err());
        assert!(mac_address_from_string("AA:BB:CC:DD:EE:G").is_err());
    }
}
