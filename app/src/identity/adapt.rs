//! Adapters from this project's two feeds to a `bt_iden` observation.
//!
//! `bt_iden` scores ten weighted features and compares the total to a threshold. Whether
//! that comparison means anything depends on which of the ten the feed could see, so the
//! adapter — not the resolver — is the place that has to say. A `BluetoothDevice` from
//! `bt_mon` has no AD structure at all unless raw payloads were enabled; a stored
//! occurrence has whatever `ble_signal_payload` kept, which for most rows written before
//! that flag existed is a name, an RSSI, and an address.
//!
//! The rule both adapters follow: **a feature gets a `Datum` only if the value handed
//! over is the datum that feature's scorer was designed for.** Everything else is
//! `Absent` and leaves the denominator (`crate::identity::replay` reports the resulting
//! coverage), and a value that stands in for the real thing is `Derived` and is discounted
//! downstream. Guessing is not free here: an unspecified datum used to default to full
//! weight, so a degraded feed looked like a healthy one exactly when it was weakest.
//!
//! # The two feeds, and what each can actually see
//!
//! | Feature | Live `BluetoothDevice` | Stored `Occurrence` |
//! |---|---|---|
//! | address | the reported MAC | `ble.address`, else six bytes of `device_hash` |
//! | RSSI | `rssi` | `ble.rssi` / the column, when it is a reading rather than `0` |
//! | name | `name` | `advertised_name`, else `ble.name` |
//! | manufacturer | `manufacturer_data` | `ble.manufacturer_data` |
//! | advertised service list | `service_data` keys — `Derived` | `ble.service_uuids` — `Derived` |
//! | layout, appearance, connectability | only with `raw_payload` | only with `ble.raw_payload_hex` |
//!
//! The service list is `Derived` on both paths because it is read off *service data*: a
//! device advertising a service without data, or a stack that has not resolved GATT yet,
//! changes that list without changing the advertisement it was scored against.
//!
//! Nothing here is wired into the live pipeline. The stored-row adapter exists so a
//! decision can be replayed and read before anything acts on it in real time.

use bt_iden::ad::AdStructures;
use bt_iden::evidence::Datum;
use bt_iden::models::{AddressType, AdvertisementObservation, BluetoothAddress};
use bt_iden::time::ObservationTime;
use bt_mon::BluetoothDevice;
use chrono::{DateTime, Utc};
use repo::models::Occurrence;
use repo::types::H3Index;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The identifier an observation carried.
///
/// The resolver compares six bytes. Most of the time those bytes are a MAC, and when
/// they are not they are the first six bytes of `occurrences.device_hash` — which is not
/// a MAC pretending to be one. It is the only identifier the row has, it rotates when a
/// platform identifier rotates, and the whole question the resolver answers is whether the
/// *other* features still say it is the same device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedIdentifier {
    /// Six bytes the address comparison runs on.
    pub bytes: [u8; 6],
    /// `ble_mac`, `uuid`, `opaque_id`, or `derived` when nothing reported an address.
    pub source: &'static str,
    /// Whether a radio reported these bytes as an address.
    ///
    /// `false` makes an exact-address match weaker evidence rather than decisive: two
    /// observations sharing a hash of a platform identifier are the same *identifier*,
    /// which is a smaller claim than being the same device across an address change.
    pub reported: bool,
}

impl ObservedIdentifier {
    /// Lowercase hex, for the report.
    pub fn hex(&self) -> String {
        hex::encode(self.bytes)
    }
}

/// One observation ready for the resolver, with the row it came from.
#[derive(Debug, Clone)]
pub struct FeedObservation {
    /// What was seen, as far as the resolver is concerned.
    pub observation: AdvertisementObservation,
    /// The identifier it arrived under.
    pub identifier: ObservedIdentifier,
    /// Wall-clock time of the observation, for reporting and for the derived tables.
    pub occurred_at: DateTime<Utc>,
    /// The node whose radio saw it, when the source names one.
    pub observer_node_id: Option<Vec<u8>>,
    /// The macro cell the observer was in, when the row was located.
    pub geo_cell_macro: Option<H3Index>,
    /// `occurrences.device_hash`, when this came from a row.
    pub device_hash: Option<Vec<u8>>,
    /// The row this was adapted from, for a report line that can be looked up.
    pub occurrence_id: Option<Uuid>,
}

/// Adapts a stored occurrence.
///
/// Returns `None` for a row with no `ble` object in its `signal_payload`: there is
/// nothing here for the identity model to consider, and inventing an observation out of a
/// row that recorded no Bluetooth data would create an identity out of nothing.
///
/// The raw advertisement bytes are applied *last*. Where both are present they and the
/// JSON fields describe one advertisement, and the bytes are the direct form of the
/// features that matter — layout, appearance, advertised service list — so they should be
/// the ones carrying the provenance.
pub fn observation_from_occurrence(occurrence: &Occurrence) -> Option<FeedObservation> {
    let ble = occurrence.signal_payload.get("ble")?;

    let identifier = identifier_from_occurrence(occurrence, ble);
    let address_type = address_type_from_payload(ble);
    // No monotonic reading is available for a row stored last month, and saying so is the
    // point: the resolver prefers whichever pair of clocks it actually has, and a replayed
    // observation only ever has one.
    let mut observation = AdvertisementObservation::new(
        ObservationTime::from_parts(occurrence.observed_at.into(), None),
        BluetoothAddress::new(identifier.bytes),
        address_type,
    );

    if let Some(name) = occurrence
        .advertised_name
        .as_deref()
        .or_else(|| ble.get("name").and_then(serde_json::Value::as_str))
        .filter(|name| !name.is_empty())
    {
        observation = observation.with_local_name_datum(name.to_string(), Datum::Direct);
    }

    // A stored `rssi` of 0 is what the capture path writes when the radio reported
    // nothing (`device.rssi.unwrap_or(0)`), not a measurement of 0 dBm. Scoring it as a
    // reading hands two devices with no signal data a perfect match on a feature neither
    // was measured on.
    if let Some(rssi) = ble
        .get("rssi")
        .and_then(serde_json::Value::as_i64)
        .or((occurrence.rssi != 0).then_some(i64::from(occurrence.rssi)))
    {
        observation = observation.with_rssi(rssi as i16);
    }

    if let Some(tx_power) = occurrence
        .tx_power
        .and_then(|value| i8::try_from(value).ok())
    {
        observation = observation.with_tx_power(tx_power);
    }

    if let Some((company_id, payload)) = manufacturer_from_payload(ble) {
        observation = observation.with_manufacturer_data(company_id, payload);
    }

    // Read out of `service_data` keys by the capture path, so it stands in for the
    // advertised service list rather than being it.
    let service_uuids: Vec<Uuid> = ble
        .get("service_uuids")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(parse_service_uuid)
                .collect()
        })
        .unwrap_or_default();
    if !service_uuids.is_empty() {
        observation = observation.with_service_uuids(service_uuids, Datum::Derived);
    }

    if let Some(bytes) = ble
        .get("raw_payload_hex")
        .and_then(serde_json::Value::as_str)
        .and_then(decode_hex)
    {
        observation = observation.with_ad_structures(&AdStructures::parse(&bytes));
    }

    Some(FeedObservation {
        observation,
        identifier,
        occurred_at: occurrence.observed_at,
        observer_node_id: Some(occurrence.origin_node_id.clone()),
        geo_cell_macro: occurrence.geo_cell_macro,
        device_hash: Some(occurrence.device_hash.clone()),
        occurrence_id: Some(occurrence.occurrence_id),
    })
}

/// Adapts a device as `bt_mon` reports it, at the instant it was read.
///
/// `seen_at` is passed in rather than taken here, so the caller pairs the wall clock with
/// the monotonic reading it read alongside this device — which is the only pairing that
/// means anything. A monotonic stamp added later has nothing to do with this
/// advertisement.
///
/// A device with no name, no manufacturer data, no raw payload, and an address that is
/// not a MAC adapts to an observation whose only feature is its identifier. That is not a
/// failure: it is the truth about what the radio saw, and the resolver's evidence floor
/// will refuse to merge on it, which is what the floor is for.
///
/// Not yet called by `FullNode`: the gap analysis is explicit that the resolver earns its
/// place in the live path only after the offline pass has been read. The adapter is here
/// and tested now so that the wiring, when it comes, is one call rather than a design.
#[allow(
    dead_code,
    reason = "the live path is deliberately unwired until the offline pass has \
                            been reviewed; see gap_analysis/DEVICE_IDENTITY.md"
)]
pub fn observation_from_device(
    device: &BluetoothDevice,
    seen_at: ObservationTime,
) -> FeedObservation {
    let identifier = identifier_from_device(device);
    let mut observation = AdvertisementObservation::new(
        seen_at,
        BluetoothAddress::new(identifier.bytes),
        // The backends report one address kind and do not say which; nothing in the
        // resolver reads this field, so the honest answer is the least specific one.
        AddressType::RandomStatic,
    );

    if let Some(name) = device.name.as_deref().filter(|name| !name.is_empty()) {
        observation = observation.with_local_name_datum(name.to_string(), Datum::Direct);
    }

    if let Some(rssi) = device.rssi {
        observation = observation.with_rssi(rssi as i16);
    }

    if let Some((&company_id, payload)) = device.manufacturer_data.iter().max_by_key(|(id, _)| *id)
    {
        // A device advertising two manufacturers in one report is not a thing the model
        // has an answer for; taking the highest id keeps the choice deterministic instead
        // of leaving it to `HashMap` order.
        observation = observation.with_manufacturer_data(company_id, payload.clone());
    }

    let service_uuids: Vec<Uuid> = device.service_data.keys().map(|key| key.0).collect();
    if !service_uuids.is_empty() {
        observation = observation.with_service_uuids(service_uuids, Datum::Derived);
    }

    if let Some(bytes) = &device.raw_payload {
        observation = observation.with_ad_structures(&AdStructures::parse(bytes));
    }

    FeedObservation {
        observation,
        identifier,
        occurred_at: Utc::now(),
        observer_node_id: None,
        geo_cell_macro: None,
        device_hash: None,
        occurrence_id: None,
    }
}

/// The identifier a row was recorded under, preferring the address it named.
fn identifier_from_occurrence(
    occurrence: &Occurrence,
    ble: &serde_json::Value,
) -> ObservedIdentifier {
    if let Some(bytes) = ble
        .get("address")
        .and_then(serde_json::Value::as_str)
        .and_then(BluetoothAddress::from_hex)
    {
        return ObservedIdentifier {
            bytes: *bytes.as_bytes(),
            source: occurrence_identifier_source(ble),
            reported: true,
        };
    }

    if let Some(bytes) = occurrence
        .device_address
        .as_deref()
        .and_then(|bytes| <[u8; 6]>::try_from(bytes).ok())
    {
        return ObservedIdentifier {
            bytes,
            source: occurrence_identifier_source(ble),
            reported: true,
        };
    }

    ObservedIdentifier {
        bytes: six_bytes_of(&occurrence.device_hash),
        source: "derived",
        reported: false,
    }
}

/// The identifier a live device arrived under.
fn identifier_from_device(device: &BluetoothDevice) -> ObservedIdentifier {
    match BluetoothAddress::from_hex(&device.address) {
        Some(address) => ObservedIdentifier {
            bytes: *address.as_bytes(),
            source: "ble_mac",
            reported: true,
        },
        None => ObservedIdentifier {
            bytes: six_bytes_of(device.id.0.as_bytes()),
            source: "opaque_id",
            reported: false,
        },
    }
}

/// `ble.id_source`, when the row says, restricted to the values `device_id.rs` writes.
fn occurrence_identifier_source(ble: &serde_json::Value) -> &'static str {
    match ble.get("id_source").and_then(serde_json::Value::as_str) {
        Some("ble_mac") => "ble_mac",
        Some("uuid") => "uuid",
        Some("opaque_id") => "opaque_id",
        // A row that recorded an address without saying where it came from still recorded
        // an address; `ble_mac` is what the capture path writes for every one it has.
        _ => "ble_mac",
    }
}

/// The six bytes a hash-based identifier is keyed by.
///
/// The whole digest is folded into six bytes rather than truncated, so two identifiers
/// that agree on the first six digest bytes but differ later do not share a key. At 48
/// bits a collision between two of a few thousand devices is on the order of one in
/// 2^32 — and the consequence is a *comparison* being made, not a merge being forced:
/// the other features still have to agree.
fn six_bytes_of(bytes: &[u8]) -> [u8; 6] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 6];
    out.copy_from_slice(&digest[..6]);
    for (index, byte) in digest.iter().enumerate().skip(6) {
        out[index % 6] ^= byte;
    }
    out
}

/// The advertised service list is stored as strings, in whichever form the writer had.
fn parse_service_uuid(value: &str) -> Option<Uuid> {
    let cleaned: String = value.chars().filter(|c| *c != '-' && *c != ':').collect();
    match cleaned.len() {
        4 => u16::from_str_radix(&cleaned, 16)
            .ok()
            .map(expand_base_uuid)
            .map(|uuid| uuid.simple().to_string())
            .and_then(|s| Uuid::parse_str(&s).ok()),
        32 => Uuid::parse_str(&cleaned).ok(),
        36 => Uuid::parse_str(value).ok(),
        _ => None,
    }
}

/// The Bluetooth Base UUID, with a 16-bit assigned number in place.
///
/// `180f` (Battery Service) and
/// `0000180f-0000-1000-8000-00805f9b34fb` are one service, and scoring them as two would
/// make a device's service list depend on which form its stack happened to print.
fn expand_base_uuid(short: u16) -> Uuid {
    let bytes = short.to_be_bytes();
    Uuid::from_bytes([
        0x00, 0x00, bytes[0], bytes[1], 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5f, 0x9b,
        0x34, 0xfb,
    ])
}

/// `ble.manufacturer_data` as the capture path writes it: `{company_id, payload}`.
fn manufacturer_from_payload(ble: &serde_json::Value) -> Option<(u16, Vec<u8>)> {
    let manufacturer = ble.get("manufacturer_data")?;
    let company_id = manufacturer
        .get("company_id")
        .and_then(serde_json::Value::as_u64)? as u16;
    let payload = match manufacturer.get("payload") {
        Some(serde_json::Value::String(hex)) => decode_hex(hex).unwrap_or_default(),
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(serde_json::Value::as_u64)
            .map(|byte| byte as u8)
            .collect(),
        _ => Vec::new(),
    };
    Some((company_id, payload))
}

/// Hex as stored, tolerating the separators each writer has preferred.
fn decode_hex(value: &str) -> Option<Vec<u8>> {
    let cleaned: String = value
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();
    if cleaned.is_empty() || !cleaned.len().is_multiple_of(2) {
        return None;
    }
    hex::decode(cleaned).ok()
}

/// `ble.address_type`, when the row recorded one.
fn address_type_from_payload(ble: &serde_json::Value) -> AddressType {
    match ble
        .get("address_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
    {
        "public" | "pub" => AddressType::Public,
        "random_static" | "static" => AddressType::RandomStatic,
        "resolvable" | "private_resolvable" => AddressType::PrivateResolvable,
        "non_resolvable" | "private_non_resolvable" => AddressType::PrivateNonResolvable,
        // The capture path writes `public` unconditionally today, which is a claim about
        // the mock beacons rather than about the radio. Nothing in the resolver reads this
        // field, so an unstated type is left at the least specific answer instead of being
        // upgraded to a fact the row does not assert.
        _ => AddressType::RandomStatic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bt_iden::evidence::Feature;
    use chrono::TimeZone;

    fn occurrence(payload: serde_json::Value) -> Occurrence {
        Occurrence::builder()
            .signal_type(repo::models::SignalType::Bluetooth)
            .origin_node_id(&[0u8; 32])
            .observed_at(Utc.timestamp_opt(1_700_000_000, 0).unwrap())
            .observed_at_node_local(Utc.timestamp_opt(1_700_000_000, 0).unwrap())
            .device_hash(&[7u8; 32])
            .signed_payload(&[0u8; 32])
            .signature(&[0u8; 64])
            .signal_payload(payload)
            .build()
    }

    #[test]
    fn a_row_with_no_bluetooth_data_adapts_to_nothing() {
        assert!(observation_from_occurrence(&occurrence(serde_json::json!({}))).is_none());
    }

    #[test]
    fn a_name_and_an_address_carry_only_their_own_evidence() {
        // This is the shape of most rows in the database, and the reason the coverage
        // floor exists: 25 of 185 points must not be able to merge two devices.
        let feed = observation_from_occurrence(&occurrence(serde_json::json!({
            "ble": {"name": "Mock Beacon 0", "address": "f0ee00000000"}
        })))
        .expect("a named row adapts");

        let sources = feed.observation.sources;
        assert_eq!(sources.get(Feature::Name), Datum::Direct);
        assert_eq!(sources.get(Feature::Address), Datum::Direct);
        for absent in [
            Feature::Appearance,
            Feature::FieldLayout,
            Feature::PayloadSimilarity,
            Feature::ManufacturerId,
            Feature::ServiceUuids,
            Feature::Connectable,
        ] {
            assert_eq!(
                sources.get(absent),
                Datum::Absent,
                "{absent:?} was credited with no evidence behind it"
            );
        }
    }

    #[test]
    fn raw_bytes_are_what_make_the_layout_real() {
        // Flags, a complete 16-bit service list, a local name, and Apple-style
        // manufacturer data: each `<len><type><data>`, so the whole advertisement is what a
        // controller with raw capture enabled would have handed over.
        let advertisement = concat!(
            "020106",                         // flags: LE general discoverable
            "03030f18",                       // complete list of 16-bit services: Battery
            "0e094d6f636b20426561636f6e2030", // complete local name: "Mock Beacon 0"
            "05ff4c000215",                   // manufacturer 0x004C, payload 02 15
        );
        let feed = observation_from_occurrence(&occurrence(serde_json::json!({
            "ble": {"name": "Mock Beacon 0", "address": "f0ee00000000",
                    "raw_payload_hex": advertisement}
        })))
        .expect("a row with advertisement bytes adapts");

        let sources = feed.observation.sources;
        assert_eq!(sources.get(Feature::FieldLayout), Datum::Direct);
        assert_eq!(sources.get(Feature::ManufacturerId), Datum::Direct);
        assert_eq!(sources.get(Feature::Name), Datum::Direct);
        assert_eq!(sources.get(Feature::Connectable), Datum::Derived);
        assert_eq!(feed.observation.field_layout, vec![0x01, 0x03, 0x09, 0xff]);
        assert_eq!(feed.observation.manufacturer_id, Some(0x004c));
        assert_eq!(
            feed.observation.local_name.as_deref(),
            Some("Mock Beacon 0")
        );
    }

    #[test]
    fn a_service_list_read_off_service_data_is_derived_not_advertised() {
        let feed = observation_from_occurrence(&occurrence(serde_json::json!({
            "ble": {"address": "f0ee00000001", "service_uuids": ["180f"]}
        })))
        .expect("adapts");
        assert_eq!(
            feed.observation.sources.get(Feature::ServiceUuids),
            Datum::Derived
        );
        assert_eq!(
            feed.observation.service_uuids,
            vec![expand_base_uuid(0x180f)],
            "a short form must be scored as the same service as its long form"
        );
    }

    #[test]
    fn short_and_long_service_uuids_are_one_service() {
        assert_eq!(
            parse_service_uuid("180f"),
            parse_service_uuid("0000180f-0000-1000-8000-00805f9b34fb")
        );
        assert_eq!(parse_service_uuid("nonsense"), None);
    }

    #[test]
    fn a_zero_rssi_is_no_reading_not_a_measurement() {
        // The capture path writes 0 when the radio reported nothing. Treating it as a
        // reading would score a feature that was never observed.
        let feed = observation_from_occurrence(&occurrence(serde_json::json!({
            "ble": {"address": "f0ee00000002"}
        })))
        .expect("adapts");
        assert_eq!(feed.observation.sources.get(Feature::Rssi), Datum::Absent);

        let with_reading = observation_from_occurrence(&occurrence(serde_json::json!({
            "ble": {"address": "f0ee00000002", "rssi": -67}
        })))
        .expect("adapts");
        assert_eq!(
            with_reading.observation.sources.get(Feature::Rssi),
            Datum::Direct
        );
        assert_eq!(with_reading.observation.rssi, -67);
    }

    #[test]
    fn a_row_with_no_address_is_keyed_on_its_hash_and_says_so() {
        let feed = observation_from_occurrence(&occurrence(serde_json::json!({
            "ble": {"name": "unknown"}
        })))
        .expect("adapts");
        assert!(!feed.identifier.reported);
        assert_eq!(feed.identifier.source, "derived");
    }

    #[test]
    fn the_derived_identifier_is_stable_and_uses_more_than_its_prefix() {
        let a = six_bytes_of(b"device-a");
        let b = six_bytes_of(b"device-a");
        assert_eq!(a, b);
        // Differing past the first six digest bytes still changes the result — without
        // the fold, two identifiers could share a key while their hashes differ.
        let mut tail = b"device-a".to_vec();
        tail.push(b'x');
        assert_ne!(a, six_bytes_of(&tail));
    }

    #[test]
    fn a_live_device_adapts_with_the_time_it_was_read() {
        let device = BluetoothDevice::new(
            bt_mon::DeviceId("f0:ee:00:00:00:09".into()),
            "f0:ee:00:00:00:09".into(),
        )
        .with_name("Live Beacon")
        .with_rssi(-55);
        let seen_at = ObservationTime::now();
        let feed = observation_from_device(&device, seen_at);

        assert_eq!(feed.identifier.source, "ble_mac");
        assert!(feed.identifier.reported);
        assert_eq!(feed.observation.sources.get(Feature::Name), Datum::Direct);
        assert_eq!(feed.observation.sources.get(Feature::Rssi), Datum::Direct);
        // No raw payload, so no layout — this is the 19 % that has no source at all.
        assert_eq!(
            feed.observation.sources.get(Feature::FieldLayout),
            Datum::Absent
        );
        assert_eq!(feed.observation.timestamp.wall(), seen_at.wall());
    }

    #[test]
    fn a_live_device_with_an_opaque_id_is_not_claimed_as_a_mac() {
        let device = BluetoothDevice::new(
            bt_mon::DeviceId("550e8400-e29b-41d4-a716-446655440000".into()),
            "550e8400-e29b-41d4-a716-446655440000".into(),
        );
        let feed = observation_from_device(&device, ObservationTime::now());
        assert_eq!(feed.identifier.source, "opaque_id");
        assert!(!feed.identifier.reported);
    }
}
