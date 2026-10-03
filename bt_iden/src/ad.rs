//! Advertising-data structures — the layer the scoring model was designed against.
//!
//! A BLE advertisement is a sequence of length-prefixed structures,
//! `<length><type><length-1 bytes of data>`, with the types defined by the SIG's
//! assigned-numbers list. Three of the resolver's features are properties of
//! *these* structures rather than of any parsed field:
//!
//! - [`Feature::FieldLayout`](crate::evidence::Feature::FieldLayout) — which
//!   structure types the advertisement contains, in the order they were sent.
//! - [`Feature::Appearance`](crate::evidence::Feature::Appearance) — the `0x19`
//!   structure's value.
//! - [`Feature::Connectable`](crate::evidence::Feature::Connectable) — read from
//!   the flags byte, with the caveat below.
//!
//! Those features have no source at all in a backend that hands over decoded
//! GATT properties, which is why [`Datum::Absent`](crate::evidence::Datum::Absent)
//! exists. Where a backend *does* expose the advertisement bytes — and where a
//! stored occurrence kept `signal_payload.ble.raw_payload_hex` — this module turns
//! those bytes into the designed datums, so the same observation type is
//! well-fed on a raw capture and honestly degraded on a parsed one.
//!
//! # Truncation
//!
//! The core specification allows the *last* structure in an advertisement to be
//! truncated when it does not fit in the 31-byte payload. Parsing follows that
//! rule: a final structure whose declared length exceeds the remaining bytes is
//! kept with the bytes that are actually there, and anything after a structure
//! that cannot be read at all is dropped. A parser that refused truncated
//! advertisements would discard precisely the busiest devices.
//!
//! # Connectability is an inference
//!
//! Whether an advertisement is connectable is a property of the PDU type
//! (`ADV_IND` vs `ADV_NONCONN_IND`), which no host stack here exposes. The flags
//! byte is the closest observable, so [`AdStructures::connectable`] answers from
//! it and a caller should record the result as
//! [`Datum::Derived`](crate::evidence::Datum::Derived), never `Direct`.
//!
//! # Example
//!
//! ```
//! use bt_iden::ad::AdStructures;
//!
//! // flags, complete local name, manufacturer specific data
//! let bytes = [
//!     0x02, 0x01, 0x06,
//!     0x06, 0x09, b'B', b'E', b'A', b'C', b'O',
//!     0x05, 0xFF, 0x4C, 0x00, 0x10, 0x05,
//! ];
//! let ad = AdStructures::parse(&bytes);
//! assert_eq!(ad.layout(), vec![0x01, 0x09, 0xFF]);
//! assert_eq!(ad.local_name().as_deref(), Some("BEACO"));
//! assert_eq!(ad.manufacturer().map(|(id, _)| id), Some(0x004C));
//! assert_eq!(ad.connectable(), Some(true));
//! ```

use std::fmt;

/// Flags (0x01).
pub const AD_FLAGS: u8 = 0x01;
/// 16-bit incomplete list of service class UUIDs (0x02).
pub const AD_UUID16_INCOMPLETE: u8 = 0x02;
/// 16-bit complete list of service class UUIDs (0x03).
pub const AD_UUID16_COMPLETE: u8 = 0x03;
/// 32-bit incomplete service class UUIDs (0x04).
pub const AD_UUID32_INCOMPLETE: u8 = 0x04;
/// 32-bit complete service class UUIDs (0x05).
pub const AD_UUID32_COMPLETE: u8 = 0x05;
/// 128-bit incomplete service class UUIDs (0x06).
pub const AD_UUID128_INCOMPLETE: u8 = 0x06;
/// 128-bit complete service class UUIDs (0x07).
pub const AD_UUID128_COMPLETE: u8 = 0x07;
/// Shortened local name (0x08).
pub const AD_NAME_SHORTENED: u8 = 0x08;
/// Complete local name (0x09).
pub const AD_NAME_COMPLETE: u8 = 0x09;
/// TX power level (0x0A).
pub const AD_TX_POWER: u8 = 0x0A;
/// Service data with a 16-bit UUID (0x16).
pub const AD_SERVICE_DATA_16: u8 = 0x16;
/// Appearance (0x19).
pub const AD_APPEARANCE: u8 = 0x19;
/// Manufacturer specific data (0xFF).
pub const AD_MANUFACTURER: u8 = 0xFF;

/// Bit 1 of the flags byte: LE General Discoverable Mode.
const FLAG_LE_GENERAL_DISCOVERABLE: u8 = 0x02;
/// Bit 3 of the flags byte: simultaneously LE and BR/EDR capable.
const FLAG_SIMULTANEOUS_LE_BREDR: u8 = 0x08;

/// One `<length><type><data>` structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdStructure {
    /// The AD type code from the SIG assigned-numbers list.
    pub kind: u8,
    /// The structure's payload, without the length and type bytes.
    ///
    /// Shorter than the declared length only for a truncated final structure.
    pub data: Vec<u8>,
}

impl AdStructure {
    /// Human-readable form of the type code, for reports.
    pub fn kind_name(&self) -> &'static str {
        describe_kind(self.kind)
    }
}

impl fmt::Display for AdStructure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#04X}({})", self.kind, self.kind_name())
    }
}

/// The structures in one advertisement, in transmission order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdStructures {
    structures: Vec<AdStructure>,
    /// Set when the final structure declared more bytes than were present.
    truncated: bool,
}

impl AdStructures {
    /// Splits advertisement bytes into structures, lenient about a truncated tail.
    pub fn parse(bytes: &[u8]) -> Self {
        let mut structures = Vec::new();
        let mut truncated = false;
        let mut cursor = 0usize;

        while cursor < bytes.len() {
            let length = bytes[cursor] as usize;
            if length == 0 {
                // Padding to the end of the 31-byte payload.
                break;
            }
            // `length` counts the type byte plus the data.
            let Some(kind) = bytes.get(cursor + 1).copied() else {
                // A length byte with no type byte behind it: a truncated tail.
                truncated = true;
                break;
            };
            let data_start = cursor + 2;
            let declared_end = cursor + 1 + length;
            let actual_end = declared_end.min(bytes.len());
            if declared_end > bytes.len() {
                truncated = true;
            }
            if data_start >= actual_end && declared_end > bytes.len() {
                // Declared data, none present: keep the type, no payload.
                structures.push(AdStructure {
                    kind,
                    data: Vec::new(),
                });
                break;
            }

            structures.push(AdStructure {
                kind,
                data: bytes[data_start..actual_end].to_vec(),
            });
            cursor = declared_end;
        }

        Self {
            structures,
            truncated,
        }
    }

    /// The structures as parsed.
    pub fn structures(&self) -> &[AdStructure] {
        &self.structures
    }

    /// `true` when nothing at all was readable.
    pub fn is_empty(&self) -> bool {
        self.structures.is_empty()
    }

    /// `true` when the tail of the advertisement was cut off.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The type codes in transmission order, deduplicated but not sorted: the
    /// ordering is itself the signal the layout feature is about.
    pub fn layout(&self) -> Vec<u8> {
        let mut seen = Vec::with_capacity(self.structures.len());
        for s in &self.structures {
            if !seen.contains(&s.kind) {
                seen.push(s.kind);
            }
        }
        seen
    }

    /// The layout as a sorted set, which is what
    /// [`HeuristicIdentityResolver`](crate::HeuristicIdentityResolver) compares:
    /// a stack may reorder structures without changing what the device is.
    pub fn layout_set(&self) -> Vec<u8> {
        let mut set = self.layout();
        set.sort_unstable();
        set
    }

    fn first_of(&self, kind: u8) -> Option<&[u8]> {
        self.structures
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| s.data.as_slice())
    }

    /// The appearance value (0x19), little-endian as transmitted.
    pub fn appearance(&self) -> Option<u16> {
        self.first_of(AD_APPEARANCE)
            .filter(|d| d.len() >= 2)
            .map(|d| u16::from_le_bytes([d[0], d[1]]))
    }

    /// The advertised TX power (0x0A), in signed dBm.
    pub fn tx_power(&self) -> Option<i8> {
        self.first_of(AD_TX_POWER)
            .and_then(|d| d.first().copied())
            .map(|byte| byte as i8)
    }

    /// The flags byte (0x01).
    pub fn flags(&self) -> Option<u8> {
        self.first_of(AD_FLAGS).and_then(|d| d.first().copied())
    }

    /// Discoverability inferred from the flags byte.
    ///
    /// `Some(true)` when the LE General Discoverable or the simultaneous
    /// LE/BR-EDR bit is set, `Some(false)` when a flags byte says otherwise, and
    /// `None` when no flags byte was advertised. The authoritative answer lives in
    /// the PDU type, which these stacks do not expose, so callers should record
    /// this as derived evidence.
    pub fn connectable(&self) -> Option<bool> {
        self.flags()
            .map(|flags| flags & (FLAG_LE_GENERAL_DISCOVERABLE | FLAG_SIMULTANEOUS_LE_BREDR) != 0)
    }

    /// The advertised local name: the complete name (0x09) if present, otherwise
    /// the shortened one (0x08).
    ///
    /// Names are UTF-8 by specification but devices do emit other bytes, so this
    /// lossy-converts rather than dropping the observation over an invalid byte.
    pub fn local_name(&self) -> Option<String> {
        self.first_of(AD_NAME_COMPLETE)
            .or_else(|| self.first_of(AD_NAME_SHORTENED))
            .filter(|d| !d.is_empty())
            .map(|d| String::from_utf8_lossy(d).into_owned())
    }

    /// The first manufacturer-specific structure (0xFF) as
    /// `(company id, remaining payload)`, the company id being little-endian.
    pub fn manufacturer(&self) -> Option<(u16, Vec<u8>)> {
        self.first_of(AD_MANUFACTURER)
            .filter(|d| d.len() >= 2)
            .map(|d| (u16::from_le_bytes([d[0], d[1]]), d[2..].to_vec()))
    }

    /// The service UUIDs advertised in 0x02/0x03 (16-bit) and 0x06/0x07 (128-bit)
    /// structures, as 128-bit values against the Bluetooth base UUID.
    ///
    /// This is the advertised *list*, which is not the same datum as the keys of a
    /// service-*data* map: a device may advertise services it carries no data for.
    pub fn service_uuids(&self) -> Vec<uuid::Uuid> {
        let mut uuids = Vec::new();
        for s in &self.structures {
            match s.kind {
                AD_UUID16_INCOMPLETE | AD_UUID16_COMPLETE => {
                    for pair in s.data.as_chunks::<2>().0 {
                        uuids.push(uuid_from_u16(u16::from_le_bytes([pair[0], pair[1]])));
                    }
                }
                AD_UUID128_INCOMPLETE | AD_UUID128_COMPLETE => {
                    for chunk in s.data.as_chunks::<16>().0 {
                        let mut bytes = [0u8; 16];
                        // Transmitted little-endian; UUIDs are big-endian.
                        for (i, b) in chunk.iter().enumerate() {
                            bytes[15 - i] = *b;
                        }
                        uuids.push(uuid::Uuid::from_bytes(bytes));
                    }
                }
                _ => {}
            }
        }
        uuids
    }

    /// The UUIDs that service-data structures (0x16) carry, which is the closest
    /// a parsed feed can get to an advertised service list — and the reason
    /// [`Datum::Derived`](crate::evidence::Datum::Derived) exists.
    pub fn service_data_uuids(&self) -> Vec<uuid::Uuid> {
        self.structures
            .iter()
            .filter(|s| s.kind == AD_SERVICE_DATA_16)
            .filter(|s| s.data.len() >= 2)
            .map(|s| uuid_from_u16(u16::from_le_bytes([s.data[0], s.data[1]])))
            .collect()
    }
}

/// Expands a 16-bit assigned UUID into the Bluetooth Base UUID form
/// `0000xxxx-0000-1000-8000-00805F9B34FB`.
fn uuid_from_u16(short: u16) -> uuid::Uuid {
    let mut bytes = [0u8; 16];
    bytes[0] = 0x00;
    bytes[1] = 0x00;
    bytes[2] = (short >> 8) as u8;
    bytes[3] = (short & 0xFF) as u8;
    bytes[4] = 0x00;
    bytes[5] = 0x00;
    bytes[6] = 0x10;
    bytes[7] = 0x00;
    bytes[8] = 0x80;
    bytes[9] = 0x00;
    bytes[10] = 0x00;
    bytes[11] = 0x80;
    bytes[12] = 0x5F;
    bytes[13] = 0x9B;
    bytes[14] = 0x34;
    bytes[15] = 0xFB;
    uuid::Uuid::from_bytes(bytes)
}

/// Names the AD types this project reads; everything else reports `unknown`.
pub fn describe_kind(kind: u8) -> &'static str {
    match kind {
        AD_FLAGS => "flags",
        AD_UUID16_INCOMPLETE => "uuid16-incomplete",
        AD_UUID16_COMPLETE => "uuid16-complete",
        AD_UUID32_INCOMPLETE => "uuid32-incomplete",
        AD_UUID32_COMPLETE => "uuid32-complete",
        AD_UUID128_INCOMPLETE => "uuid128-incomplete",
        AD_UUID128_COMPLETE => "uuid128-complete",
        AD_NAME_SHORTENED => "name-shortened",
        AD_NAME_COMPLETE => "name-complete",
        AD_TX_POWER => "tx-power",
        AD_SERVICE_DATA_16 => "service-data-16",
        AD_APPEARANCE => "appearance",
        AD_MANUFACTURER => "manufacturer",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_structures_in_transmission_order() {
        let bytes = [
            0x02, 0x01, 0x06, // flags
            0x03, 0x03, 0xAA, 0xFE, // 16-bit complete service UUID list
            0x04, 0xFF, 0x4C, 0x00, 0x01, // manufacturer
        ];
        let ad = AdStructures::parse(&bytes);
        assert_eq!(
            ad.structures().iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![AD_FLAGS, AD_UUID16_COMPLETE, AD_MANUFACTURER]
        );
        assert_eq!(ad.layout(), vec![0x01, 0x03, 0xFF]);
        assert!(!ad.truncated());
    }

    #[test]
    fn keeps_a_truncated_final_structure_with_the_bytes_present() {
        // Declares 6 bytes of name and offers 3.
        let bytes = [0x02, 0x01, 0x06, 0x07, 0x09, b'a', b'b', b'c'];
        let ad = AdStructures::parse(&bytes);
        assert!(ad.truncated());
        assert_eq!(ad.local_name().as_deref(), Some("abc"));
    }

    #[test]
    fn stops_at_a_length_byte_with_nothing_behind_it() {
        let bytes = [0x02, 0x01, 0x06, 0x03];
        let ad = AdStructures::parse(&bytes);
        assert_eq!(ad.structures().len(), 1);
        assert!(ad.truncated());
    }

    #[test]
    fn padding_terminates_the_parse() {
        let bytes = [0x02, 0x01, 0x06, 0x00, 0x00, 0x00];
        let ad = AdStructures::parse(&bytes);
        assert_eq!(ad.structures().len(), 1);
        assert!(!ad.truncated());
    }

    #[test]
    fn reads_appearance_little_endian() {
        let bytes = [0x03, 0x19, 0xC0, 0x01]; // 0x01C0 = 448, generic phone
        let ad = AdStructures::parse(&bytes);
        assert_eq!(ad.appearance(), Some(448));
    }

    #[test]
    fn reads_tx_power_as_a_signed_byte() {
        let bytes = [0x02, 0x0A, 0xF6]; // -10 dBm
        assert_eq!(AdStructures::parse(&bytes).tx_power(), Some(-10));
    }

    #[test]
    fn connectability_comes_from_the_flags_and_absent_flags_says_nothing() {
        assert_eq!(
            AdStructures::parse(&[0x02, 0x01, 0x06]).connectable(),
            Some(true)
        );
        assert_eq!(
            AdStructures::parse(&[0x02, 0x01, 0x04]).connectable(),
            Some(false)
        );
        assert_eq!(AdStructures::parse(&[0x02, 0x09, b'x']).connectable(), None);
    }

    #[test]
    fn complete_name_wins_over_shortened() {
        let bytes = [
            0x04, 0x08, b'a', b'b', b'c', // shortened "abc"
            0x04, 0x09, b'x', b'y', b'z', // complete "xyz"
        ];
        assert_eq!(
            AdStructures::parse(&bytes).local_name().as_deref(),
            Some("xyz")
        );
    }

    #[test]
    fn manufacturer_id_is_little_endian_and_the_payload_follows_it() {
        let bytes = [0x06, 0xFF, 0x4C, 0x00, 0x10, 0x05, 0x01];
        let (id, payload) = AdStructures::parse(&bytes).manufacturer().unwrap();
        assert_eq!(id, 0x004C);
        assert_eq!(payload, vec![0x10, 0x05, 0x01]);
    }

    #[test]
    fn short_service_uuids_expand_against_the_bluetooth_base_uuid() {
        let bytes = [0x05, 0x03, 0xAA, 0xFE, 0x0B, 0x12];
        let uuids = AdStructures::parse(&bytes).service_uuids();
        assert_eq!(uuids.len(), 2);
        assert_eq!(uuids[0].to_string(), "0000feaa-0000-1000-8000-00805f9b34fb");
    }

    #[test]
    fn long_service_uuids_are_read_back_in_uuid_byte_order() {
        let mut bytes = vec![0x11, 0x07];
        // 128-bit UUID transmitted little-endian: A0 A1 ... AF.
        let transmitted: [u8; 16] = std::array::from_fn(|i| 0xA0 + i as u8);
        bytes.extend_from_slice(&transmitted);

        let uuids = AdStructures::parse(&bytes).service_uuids();
        assert_eq!(uuids.len(), 1);
        let mut expected = transmitted;
        expected.reverse();
        assert_eq!(uuids[0].as_bytes(), &expected);
    }

    #[test]
    fn service_data_uuids_are_a_separate_datum_from_the_advertised_list() {
        let bytes = [0x03, 0x16, 0xAA, 0xFE, 0x01];
        let ad = AdStructures::parse(&bytes);
        assert!(ad.service_uuids().is_empty());
        assert_eq!(ad.service_data_uuids().len(), 1);
    }

    #[test]
    fn layout_set_is_sorted_and_deduplicated() {
        let bytes = [
            0x02, 0x01, 0x06, // flags
            0x04, 0xFF, 0x4C, 0x00, 0x01, // manufacturer
            0x02, 0x01, 0x04, // flags again
        ];
        let ad = AdStructures::parse(&bytes);
        assert_eq!(ad.layout(), vec![0x01, 0xFF]);
        assert_eq!(ad.layout_set(), vec![0x01, 0xFF]);
    }

    #[test]
    fn an_empty_advertisement_parses_to_nothing_rather_than_failing() {
        let ad = AdStructures::parse(&[]);
        assert!(ad.is_empty());
        assert!(!ad.truncated());
    }

    #[test]
    fn a_name_with_invalid_utf8_is_kept_lossily() {
        let bytes = [0x04, 0x09, 0xFF, 0xFE, 0x41];
        let name = AdStructures::parse(&bytes).local_name().unwrap();
        assert!(name.ends_with('A'));
    }

    #[test]
    fn known_types_are_named_for_reports() {
        assert_eq!(describe_kind(0x09), "name-complete");
        assert_eq!(describe_kind(0x19), "appearance");
        assert_eq!(describe_kind(0x7F), "unknown");
        assert_eq!(
            AdStructures::parse(&[0x02, 0x19, 0x00]).structures()[0].to_string(),
            "0x19(appearance)"
        );
    }
}
