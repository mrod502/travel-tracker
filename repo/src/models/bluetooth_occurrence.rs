use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Represents a single Bluetooth device advertisement occurrence
///
/// This is an append-only record of a Bluetooth device being observed by a node.
/// The table does not support updates - each occurrence is a unique event.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct BluetoothOccurrence {
    /// UUIDv7 - time-sortable unique identifier for this occurrence
    pub occurrence_id: String,

    /// ID of the node that observed this device
    pub node_id: String,

    /// UTC timestamp after clock sync correction
    pub observed_at: DateTime<Utc>,

    /// Raw node-local timestamp before sync correction (for drift auditing)
    pub observed_at_node_local: Option<DateTime<Utc>>,

    // --- Device Information ---

    /// BLE MAC address (may be randomized/rotating)
    pub device_address: String,

    /// Address type: public, random_static, random_resolvable, random_nonresolvable
    pub device_address_type: Option<String>,

    /// Device name from AD payload (if present)
    pub device_advertised_name: Option<String>,

    /// Stable pseudonymous ID for privacy (SHA256 hash of address)
    pub device_hash: Option<String>,

    // --- Advertisement Details ---

    /// Advertisement type: ADV_IND, ADV_NONCONN_IND, SCAN_RSP, etc.
    pub advertisement_type: Option<String>,

    /// Signal strength in dBm
    pub rssi: Option<i32>,

    /// TX power from AD payload (if present)
    pub tx_power: Option<i32>,

    /// Advertised service UUIDs
    pub service_uuids: Option<Vec<String>>,

    /// Manufacturer data company ID (hex string, e.g., "0x004C")
    pub manufacturer_company_id: Option<String>,

    /// Raw manufacturer data payload (hex string)
    pub manufacturer_payload_hex: Option<String>,

    /// Full raw advertisement payload (hex string) for reprocessing
    pub raw_payload_hex: Option<String>,

    // --- Location Data ---

    /// Latitude (NUMERIC(10,7) - ~1cm accuracy)
    pub location_lat: Option<Decimal>,

    /// Longitude (NUMERIC(11,7) - ~1cm accuracy)
    pub location_lon: Option<Decimal>,

    /// Altitude in meters (NUMERIC(8,3) - 1mm precision)
    pub location_alt_m: Option<Decimal>,

    /// GPS fix accuracy in meters (NUMERIC(6,3))
    pub location_accuracy_m: Option<Decimal>,

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
    /// * `occurrence_id` - UUIDv7 identifier (use `uuid::Uuid::now_v7()`)
    /// * `node_id` - ID of the observing node
    /// * `observed_at` - UTC timestamp after clock sync
    /// * `device_address` - BLE MAC address
    ///
    /// # Returns
    /// A new `BluetoothOccurrence` with default values for optional fields
    pub fn new(
        occurrence_id: String,
        node_id: String,
        observed_at: DateTime<Utc>,
        device_address: String,
    ) -> Self {
        Self {
            occurrence_id,
            node_id,
            observed_at,
            observed_at_node_local: None,
            device_address,
            device_address_type: None,
            device_advertised_name: None,
            device_hash: None,
            advertisement_type: None,
            rssi: None,
            tx_power: None,
            service_uuids: None,
            manufacturer_company_id: None,
            manufacturer_payload_hex: None,
            raw_payload_hex: None,
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

    /// Set the device hash for privacy
    pub fn with_device_hash(mut self, hash: impl Into<String>) -> Self {
        self.device_hash = Some(hash.into());
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

    /// Set service UUIDs
    pub fn with_service_uuids(mut self, uuids: Vec<String>) -> Self {
        self.service_uuids = Some(uuids);
        self
    }

    /// Set manufacturer data
    pub fn with_manufacturer_data(
        mut self,
        company_id: impl Into<String>,
        payload_hex: impl Into<String>,
    ) -> Self {
        self.manufacturer_company_id = Some(company_id.into());
        self.manufacturer_payload_hex = Some(payload_hex.into());
        self
    }

    /// Set raw payload
    pub fn with_raw_payload(mut self, payload_hex: impl Into<String>) -> Self {
        self.raw_payload_hex = Some(payload_hex.into());
        self
    }

    /// Set location coordinates
    pub fn with_location(
        mut self,
        lat: Decimal,
        lon: Decimal,
        alt_m: Option<Decimal>,
        accuracy_m: Option<Decimal>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_occurrence() {
        let occurrence = BluetoothOccurrence::new(
            "01J9XYZ123456789".to_string(),
            "node-001".to_string(),
            Utc::now(),
            "AA:BB:CC:DD:EE:FF".to_string(),
        );

        assert_eq!(occurrence.node_id, "node-001");
        assert_eq!(occurrence.device_address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(occurrence.schema_version, 1);
        assert!(occurrence.observed_at_node_local.is_none());
    }

    #[test]
    fn test_builder_pattern() {
        let occurrence = BluetoothOccurrence::new(
            "01J9XYZ123456789".to_string(),
            "node-001".to_string(),
            Utc::now(),
            "AA:BB:CC:DD:EE:FF".to_string(),
        )
        .with_rssi(-67)
        .with_advertisement_type("ADV_IND")
        .with_advertised_name("Device-1234");

        assert_eq!(occurrence.rssi, Some(-67));
        assert_eq!(occurrence.advertisement_type, Some("ADV_IND".to_string()));
        assert_eq!(
            occurrence.device_advertised_name,
            Some("Device-1234".to_string())
        );
    }
}
