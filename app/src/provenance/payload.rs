//! Canonical payload structures for cryptographic signing.
//!
//! Every signed document starts with `schema_version`, which says which shape
//! follows. Two shapes exist, and both are readable:
//!
//! | version | what the signature covers | written by |
//! |---------|---------------------------|------------|
//! | [`VERSION_V1`] | `signal_type`, the two node/device ids, `device_address`, `observed_at_node_local`, `rssi`, `location` — **a subset of the row** | nodes before the v2 change |
//! | [`VERSION_V2`] | v1's set plus `observed_at`, `alt_m`, `accuracy_m`, `location_source`, `geo_cell_fine`, `geo_cell_macro`, `signal_payload`, `advertised_name` — every column the node authors | current nodes |
//!
//! # Why v2 exists
//!
//! A signature is worth exactly as much as the set of columns it covers. In v1 the
//! row's richest columns sit outside the signed bytes: [`PayloadV2::signal_payload`]
//! (the advertisement contents *and* the `position` provenance block the node adds),
//! [`PayloadV2::advertised_name`] (the column most likely to carry personal data),
//! `tx_power`, `adv_type`, the altitude and accuracy of the fix, and both derived H3
//! cells. Any of them could be rewritten after the fact and the signature would still
//! verify. v2 signs them all.
//!
//! v1 stays decodable ([`encode::decode_payload`](super::encode::decode_payload)
//! dispatches on the version), so rows already in a database remain verifiable —
//! [`CanonicalPayload::covers_row`] reports that they attest to less than the row.
//!
//! # What is deliberately still outside the signature
//!
//! - `occurrence_id`. The node does not author the id in a way an auditor can
//!   re-derive ([GAP_ANALYSIS M21]), so binding it would attest to a value whose
//!   provenance is the database's, not the node's.
//! - `ingested_at`. When *this* database wrote the row is a claim of the writer, not
//!   of the origin node, and a relay writes it on the aggregator's behalf.
//! - `rssi`'s absence. The column is `SMALLINT NOT NULL`, so a missing reading is
//!   still recorded as 0 dBm and v2 signs that fabrication faithfully
//!   ([GAP_ANALYSIS M20]). Fixing it means changing the column, not the payload.
//! - Location added by an aggregator. It was outside v1 and stays outside v2: the
//!   origin node cannot attest to a position it did not measure.
//!
//! # Timestamps
//!
//! v2 signs both timestamps at **microsecond** precision
//! ([`canonical_timestamp`]). `TIMESTAMPTZ` stores microseconds, while
//! `DateTime::to_rfc3339()` prints nanoseconds when the value has them — so a v1
//! timestamp could not be reproduced from the column it came from, which made
//! row ↔ proof cross-checking impossible regardless of which columns were signed
//! ([GAP_ANALYSIS M19]). v1's nanosecond spelling is frozen: those bytes are what
//! was signed and are never re-encoded.
//!
//! # Wire shape
//!
//! [`VERSION_V2`] is a **positional CBOR array** of [`V2_ELEMENT_COUNT`] elements — the
//! layout the architecture spec
//! ([`canonical-cbor-spec.md`](../../../.knowledge/architecture/canonical-cbor-spec.md))
//! prescribes, whose first rule is that a signed payload should not be a map. An
//! element's meaning is its index; binary fields are CBOR byte strings (major type 2),
//! not arrays of integers; a known enum is a code from a table in this module rather
//! than a spelling. Because the document carries no keys there is no key order for two
//! implementations to disagree about, which makes the encoding RFC 8949 §4.2.1
//! *core-deterministic* rather than merely reproducible — `ciborium`'s writer emits
//! minimal integer heads and the shortest float that round-trips bit-exactly
//! ([GAP_ANALYSIS B8/B9](../../../GAP_ANALYSIS.md#81-blocking)).
//!
//! [`VERSION_V1`] stays a text-keyed map whose keys are in Rust field-declaration order:
//! *deterministic*, not §4.2.1. Those bytes were signed by nodes that no longer run this
//! code, so the shape is frozen — it is decoded, never re-encoded, and rewriting it would
//! invalidate every v1 signature on record.
//!
//! A v2 decoder is strict about the shape: exactly [`V2_ELEMENT_COUNT`] elements, a byte
//! string wherever the layout says bytes (an array of integers is refused even though
//! `ciborium` would otherwise accept one as bytes), a code inside each enum table, and
//! nothing after the document ends. [`encode::decode_payload`](super::encode::decode_payload)
//! is that path.

use chrono::{DateTime, Timelike, Utc};
use repo::models::enums::{AdvType, LocationSource};
use repo::models::SignalType;
use repo::{geo, CellIndex, Occurrence, PostgisPoint};
use thiserror::Error;

/// Payload version 1: the signed set is a subset of the row.
pub const VERSION_V1: u16 = 1;

/// Payload version 2: every column the node authors is inside the signature.
pub const VERSION_V2: u16 = 2;

/// The version this build signs new occurrences with.
pub const CURRENT_VERSION: u16 = VERSION_V2;

/// Errors from deriving a payload from a row.
#[derive(Debug, Error)]
pub enum PayloadError {
    /// The `signal_payload` column could not be put into its canonical byte form.
    #[error("signal payload could not be encoded as canonical JSON: {0}")]
    SignalPayload(#[from] serde_json::Error),

    /// The stored location has no H3 cell, so the cells the signature would name
    /// cannot be computed.
    #[error("location ({lat}, {lon}) has no H3 cell: {source}")]
    Geo {
        lat: f64,
        lon: f64,
        #[source]
        source: geo::GeoError,
    },
}

/// The only timestamp spelling that survives being stored.
///
/// Microsecond precision, fixed width, UTC — `2026-09-15T12:00:00.123456+00:00`.
/// Six digits are always printed, including for a whole second (`.000000`), so the
/// string does not depend on how many trailing digits the value happened to carry.
///
/// Nanoseconds are **truncated**, not rounded: rounding would move the instant, and
/// the point of the field is that it names the same instant the column holds.
/// Pair it with [`truncate_to_micros`], which applies the same truncation to the
/// value going into the row, so the signed string and the stored column agree without
/// anyone having to reason about what the database does to a nanosecond on insert.
pub fn canonical_timestamp(value: DateTime<Utc>) -> String {
    truncate_to_micros(value)
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

/// Drop the sub-microsecond part of an instant.
///
/// The row and the signature have to name the same instant, and `TIMESTAMPTZ` can
/// only hold microseconds. Truncating here — once, before either is built — means
/// neither has to guess what the other did.
pub fn truncate_to_micros(value: DateTime<Utc>) -> DateTime<Utc> {
    let micros = value.nanosecond() - (value.nanosecond() % 1_000);
    // Truncation only ever lowers the nanosecond within the same second, so it stays
    // in range — including across a leap second, whose nanoseconds exceed 1e9.
    value.with_nanosecond(micros).unwrap_or(value)
}

/// A signed payload, in whichever version was signed.
///
/// Reading a stored row means reading a payload whose version nobody chose: an
/// old database has v1 rows, a node that has been upgraded writes v2. The variants
/// keep that explicit rather than pretending the shapes are one struct.
#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalPayload {
    /// Version 1 — attests to a subset of the row.
    V1(PayloadV1),
    /// Version 2 — attests to every column the node authors.
    V2(PayloadV2),
}

impl CanonicalPayload {
    /// The `schema_version` this document declares.
    pub fn version(&self) -> u16 {
        match self {
            CanonicalPayload::V1(_) => VERSION_V1,
            CanonicalPayload::V2(_) => VERSION_V2,
        }
    }

    /// True when the signature covers every column of the row it belongs to.
    ///
    /// The distinction an auditor actually needs: a valid v1 signature proves the
    /// origin node signed *something about this observation*, and nothing about the
    /// device name, the advertisement contents, the altitude, or the derived cells.
    /// A v2 signature proves those too.
    pub fn covers_row(&self) -> bool {
        matches!(self, CanonicalPayload::V2(_))
    }

    /// The signal type code (0 = bluetooth, 1 = wifi, …).
    pub fn signal_type(&self) -> u8 {
        match self {
            CanonicalPayload::V1(p) => p.signal_type,
            CanonicalPayload::V2(p) => p.signal_type,
        }
    }

    /// Signal type as the name the code table gives it.
    pub fn signal_type_str(&self) -> &'static str {
        match self {
            CanonicalPayload::V1(p) => p.signal_type_str(),
            CanonicalPayload::V2(p) => p.signal_type_str(),
        }
    }

    /// The 32-byte node id the signature claims.
    pub fn origin_node_id(&self) -> &[u8] {
        match self {
            CanonicalPayload::V1(p) => &p.origin_node_id,
            CanonicalPayload::V2(p) => &p.origin_node_id,
        }
    }

    /// The 32-byte device pseudonym.
    pub fn device_hash(&self) -> &[u8] {
        match self {
            CanonicalPayload::V1(p) => &p.device_hash,
            CanonicalPayload::V2(p) => &p.device_hash,
        }
    }

    /// The raw device address, when the backend exposed one.
    pub fn device_address(&self) -> Option<&[u8]> {
        match self {
            CanonicalPayload::V1(p) => p.device_address.as_deref(),
            CanonicalPayload::V2(p) => p.device_address.as_deref(),
        }
    }

    /// RSSI in dBm as signed.
    pub fn rssi(&self) -> i16 {
        match self {
            CanonicalPayload::V1(p) => p.rssi,
            CanonicalPayload::V2(p) => p.rssi,
        }
    }

    /// The claimed location as `[lat, lon]`.
    pub fn location(&self) -> Option<[f64; 2]> {
        match self {
            CanonicalPayload::V1(p) => p.location,
            CanonicalPayload::V2(p) => p.location,
        }
    }

    /// The raw, pre-correction node-local timestamp.
    pub fn observed_at_node_local(&self) -> &str {
        match self {
            CanonicalPayload::V1(p) => &p.observed_at_node_local,
            CanonicalPayload::V2(p) => &p.observed_at_node_local,
        }
    }

    /// The sync-corrected timestamp — `None` for v1, which leaves the column the
    /// partition key and the time a query returns unsigned.
    pub fn observed_at(&self) -> Option<&str> {
        match self {
            CanonicalPayload::V1(_) => None,
            CanonicalPayload::V2(p) => Some(&p.observed_at),
        }
    }

    /// The name the device advertised, when it advertised one.
    pub fn advertised_name(&self) -> Option<&str> {
        match self {
            CanonicalPayload::V1(p) => p.advertised_name.as_deref(),
            CanonicalPayload::V2(p) => p.advertised_name.as_deref(),
        }
    }

    /// The signed signal-payload bytes — always present in v2, always `None` in
    /// v1 as written (the field exists in the v1 shape and no producer ever set
    /// it, which is the defect v2 exists to close).
    pub fn signed_signal_payload(&self) -> Option<&[u8]> {
        match self {
            CanonicalPayload::V1(p) => p.signal_payload.as_deref(),
            CanonicalPayload::V2(p) => Some(&p.signal_payload),
        }
    }

    /// The `adv_type` column label the signature claims, when it claims one.
    ///
    /// Reads the element through the code table so that nothing downstream of here
    /// compares a raw number against a column value. v1 stores the code without a
    /// table of its own, so it reports `None` rather than borrowing v2's mapping for
    /// a document v2 never signed.
    pub fn adv_type_label(&self) -> Option<&'static str> {
        match self {
            CanonicalPayload::V1(_) => None,
            CanonicalPayload::V2(p) => p.adv_type.and_then(self::adv_type_label),
        }
    }

    /// The `location_source` column label the signature claims.
    ///
    /// `None` for v1, which leaves the column unsigned.
    pub fn location_source_label(&self) -> Option<&'static str> {
        match self {
            CanonicalPayload::V1(_) => None,
            CanonicalPayload::V2(p) => self::location_source_label(p.location_source),
        }
    }
}

impl From<PayloadV1> for CanonicalPayload {
    fn from(payload: PayloadV1) -> Self {
        CanonicalPayload::V1(payload)
    }
}

impl From<PayloadV2> for CanonicalPayload {
    fn from(payload: PayloadV2) -> Self {
        CanonicalPayload::V2(payload)
    }
}

/// Payload version 1 — **frozen**.
///
/// The field set and their types are exactly what nodes have already signed, so
/// nothing here may change: stored bytes are verified as they are, and a shape
/// change would silently invalidate every v1 row. Extend [`PayloadV2`] instead.
///
/// The `tx_power`, `adv_type`, `signal_payload` and `advertised_name` fields are
/// part of the v1 shape but no producer ever set them, which is why they are
/// declared and unsigned at the same time.
///
/// # Field order
///
/// `ciborium` emits keys in declaration order, so the order below is part of the
/// signed format: reordering these fields changes every signature produced
/// afterwards without changing a single value.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PayloadV1 {
    /// Schema version, always 1. MUST be first for version detection.
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
    /// The sync-corrected timestamp is stored separately in the database, unsigned.
    pub observed_at_node_local: String,

    /// RSSI value in dBm.
    ///
    /// Signed 16-bit integer to accommodate typical RSSI ranges (-128 to +127).
    pub rssi: i16,

    /// TX power from advertisement payload (if present).
    ///
    /// Optional because not all devices advertise TX power. Declared in v1 and
    /// never set by any producer.
    pub tx_power: Option<i16>,

    /// BLE advertisement type (if Bluetooth signal).
    ///
    /// Optional because only applicable to Bluetooth signals. Declared in v1 and
    /// never set by any producer.
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
    /// Declared in v1 and never set, so the `signal_payload` column — the BLE
    /// advertisement contents and the position provenance block — was unsigned.
    pub signal_payload: Option<Vec<u8>>,

    /// Advertised device name (if present in advertisement).
    ///
    /// Declared in v1 and never set.
    pub advertised_name: Option<String>,
}

impl PayloadV1 {
    /// Create a v1 payload builder.
    pub fn builder() -> PayloadV1Builder {
        PayloadV1Builder::new()
    }

    /// Get the signal type as a string representation.
    pub fn signal_type_str(&self) -> &'static str {
        signal_type_str(self.signal_type)
    }
}

/// Payload version 2 — the signature covers the row.
///
/// One field per column the node authors, each carrying the stored value in the
/// form the signature specifies — raw bytes for the byte columns, the code tables
/// below for the two enum columns, the column's own `REAL`/`SMALLINT` widths for the
/// rest — so a verifier can compare the attestation against the row and check the
/// result mechanically rather than by eye:
///
/// The wire form is a **CBOR array of 18 elements**, positional, in the order the
/// [architecture spec](../../../.knowledge/architecture/canonical-cbor-spec.md)
/// gives: the twelve elements the spec indexes 0-11 keep those indices, and the six
/// this version adds to cover the row are appended at the end, which is the
/// extension rule the same spec states ("new field added at end").
///
/// | # | payload field | column | CBOR type |
/// |---|---------------|--------|-----------|
/// | 0 | `schema_version` | `schema_version` | uint, always 2 |
/// | 1 | `signal_type` | `signal_type` | uint, the code [below](#signal-type) |
/// | 2 | `origin_node_id` | `origin_node_id` | **bytes** (32) |
/// | 3 | `device_hash` | `device_hash` | **bytes** (32) |
/// | 4 | `device_address` | `device_address` | **bytes** (6 for a BLE MAC) or null |
/// | 5 | `observed_at_node_local` | `observed_at_node_local` | text, microsecond RFC 3339 UTC |
/// | 6 | `rssi` | `rssi` | int, dBm |
/// | 7 | `tx_power` | `tx_power` | int, dBm, or null |
/// | 8 | `adv_type` | `adv_type` | uint code ([table](#adv-type)) or null |
/// | 9 | `location` | `location` | array `[lat, lon]` of float, or null |
/// | 10 | `signal_payload` | `signal_payload` | **bytes** — never null |
/// | 11 | `advertised_name` | `advertised_name` | text or null |
/// | 12 | `observed_at` | `observed_at` | text, microsecond RFC 3339 UTC |
/// | 13 | `alt_m` | `alt_m` | float (the column is `REAL`), or null |
/// | 14 | `accuracy_m` | `accuracy_m` | float (the column is `REAL`), or null |
/// | 15 | `location_source` | `location_source` | uint code ([table](#location-source)) |
/// | 16 | `geo_cell_fine` | `geo_cell_fine` | uint (resolution 9 cell), or null |
/// | 17 | `geo_cell_macro` | `geo_cell_macro` | uint (resolution 6 cell), or null |
///
/// `observed_at` is inside v2's signature on purpose. Leaving it out was defensible
/// while the corrected time was a database-side claim, but nothing in the system
/// recorded who made it, so whoever wrote the row also chose — undetectably — which
/// partition it landed in and what time a query returns for it. With it signed, the
/// correction is attributable to the origin node.
///
/// # Element order
///
/// The order in the table above is the wire order, and it is written down twice:
/// once here as the twelve spec elements followed by the six appended ones, and once
/// in the [`Serialize`](#impl-Serialize-for-PayloadV2) impl, which emits the elements
/// in that order. Declaration order is kept identical to both so that reading the
/// struct is reading the format.
///
/// `Serialize`/`Deserialize` are hand-written rather than derived because the derived
/// form of a struct is a CBOR *map*, and this format has no keys: an element is
/// identified by its index alone, which is what removes the key-ordering question
/// [RFC 8949 §4.2.1](https://datatracker.ietf.org/doc/html/rfc8949#section-4.2.1)
/// would otherwise raise, and leaves no room for a document that omits an element
/// instead of nulling it.
#[derive(Debug, Clone, PartialEq)]
pub struct PayloadV2 {
    /// Element 0 — schema version, always [`VERSION_V2`].
    pub schema_version: u16,

    /// Element 1 — type of signal being reported; same code table as v1.
    pub signal_type: u8,

    /// Element 2 — origin node identity: the 32-byte SHA-256 hash of the signing
    /// public key, as a CBOR byte string.
    pub origin_node_id: Vec<u8>,

    /// Element 3 — device pseudonymous ID: the 32-byte SHA-256 hash of the device
    /// address, as a CBOR byte string.
    pub device_hash: Vec<u8>,

    /// Element 4 — raw MAC/address (6 bytes for BLE/WiFi, variable for others), or
    /// null. A CBOR byte string when present.
    pub device_address: Option<Vec<u8>>,

    /// Element 5 — the raw node-local time, `observed_at_node_local`, at microsecond
    /// precision.
    ///
    /// The drift-audit pair of [`observed_at`](Self::observed_at): the two together
    /// say what the node's clock read and what the correction made of it.
    pub observed_at_node_local: String,

    /// Element 6 — RSSI value in dBm.
    pub rssi: i16,

    /// Element 7 — TX power from the advertisement payload, when the device
    /// advertised it.
    pub tx_power: Option<i16>,

    /// Element 8 — the `adv_type` column's value as a canonical code, when the
    /// capture layer reported one.
    ///
    /// The spec calls for a byte value here rather than the enum's text label, and
    /// the code table is [`adv_type_code`] with its inverse [`adv_type_label`]. A
    /// number is what makes the element checkable: 0 through 3 are the only
    /// assignments, so a decoder can refuse a code it does not know instead of
    /// storing a label no column can hold. [`CanonicalPayload::adv_type_label`]
    /// reads the label back for a human.
    pub adv_type: Option<u8>,

    /// Element 9 — location if from the origin node, as a two-element array
    /// `[lat, lon]` of float.
    ///
    /// Aggregator-added location is still NOT covered by this signature.
    pub location: Option<[f64; 2]>,

    /// Element 10 — the `signal_payload` column's JSON value in canonical bytes.
    ///
    /// The column is `JSONB NOT NULL`, so unlike v1's optional field this is always
    /// signed — including an empty object, which is a claim that the node recorded
    /// nothing signal-specific rather than that nothing was signed.
    ///
    /// The bytes are `serde_json`'s compact serialisation with object keys in
    /// `serde_json::Map` order, which is what makes them reproducible from the
    /// column: JSONB reorders keys and normalises whitespace on the way in, and both
    /// are invisible to this form. Numbers survive too — JSONB stores a `numeric` and
    /// prints its own spelling, but re-parsing lands on the same value, so
    /// re-serialising gives the signed bytes back
    /// ([`encode::canonical_signal_payload`](super::encode::canonical_signal_payload)
    /// names the test). What does not survive is comparing the column's *text* against
    /// the signed bytes, or a verifier whose JSON keeps number text with more precision
    /// than this one.
    ///
    /// Inside CBOR these bytes are an opaque byte string (major type 2): CBOR has no
    /// opinion about the JSON inside them, and the rules above are about reproducing
    /// the bytes, not about encoding the JSON as CBOR.
    pub signal_payload: Vec<u8>,

    /// Element 11 — advertised device name, when the device advertised one.
    ///
    /// The column this whole change is most concerned with: it is the row's most
    /// likely carrier of personal data, and in v1 it could be edited freely.
    pub advertised_name: Option<String>,

    /// Element 12 — the sync-corrected observation time, `observed_at`, at
    /// microsecond precision. Appended for this version; see the note above on why
    /// the correction is inside the signature.
    pub observed_at: String,

    /// Element 13 — altitude above mean sea level in metres, `alt_m` (`REAL`, so
    /// f32). Appended for this version.
    pub alt_m: Option<f32>,

    /// Element 14 — horizontal accuracy estimate in metres, `accuracy_m` (`REAL`, so
    /// f32). Appended for this version.
    ///
    /// f32 rather than the f64 a position fix carries, because the column is `REAL`
    /// and signing the wider value would sign something the row cannot show.
    pub accuracy_m: Option<f32>,

    /// Element 15 — the `location_source` column's value as a canonical code.
    /// Appended for this version.
    ///
    /// The column is `NOT NULL`, so this element is always present — the same
    /// reasoning as [`adv_type`](Self::adv_type) applies to the code table
    /// ([`location_source_code`] / [`location_source_label`]). Where a row has no
    /// location at all the schema still holds a label, and the signature attests to
    /// the code for the label that is actually stored; `location: None` is what says
    /// no position was claimed.
    pub location_source: u8,

    /// Element 16 — resolution 9 H3 cell derived from `location`, as a uint.
    /// Appended for this version.
    ///
    /// A generated column nobody can write directly, which is exactly why it is
    /// worth signing: it pins the cell the node derived from the location it
    /// measured, so a reader can check the column against the claim rather than
    /// trusting that whoever regenerated it used the same h3 build.
    pub geo_cell_fine: Option<u64>,

    /// Element 17 — resolution 6 H3 cell, the parent of
    /// [`geo_cell_fine`](Self::geo_cell_fine), as a uint. Appended for this version.
    pub geo_cell_macro: Option<u64>,
}

/// The number of elements in a v2 signed array.
///
/// Fixed, and fixed at 18: an element is identified by index alone, so a document
/// with 17 or 19 is not a v2 payload with something missing or added — it is not a v2
/// payload at all. A later version that needs another element bumps the version
/// rather than the count.
pub const V2_ELEMENT_COUNT: usize = 18;

/// A byte slice on its way to the wire as a CBOR byte string.
///
/// `serde` serialises a bare `Vec<u8>` as a *sequence* of integers, and ciborium will
/// decode either spelling back into `Vec<u8>`, which is exactly the ambiguity [GAP
/// ANALYSIS B9](../../../GAP_ANALYSIS.md#81-blocking) exists to remove: `origin_node_id`
/// as `[0, 127, 4, ...]` and as `48 7F 04 ...` are different byte strings, would carry
/// different signatures, and must not both be readable as the same claim. Going
/// through this type means the encoder calls `serialize_bytes` (major type 2) and the
/// decoder refuses anything else.
struct ByteStr<'a>(&'a [u8]);

struct ByteBuf(Vec<u8>);

impl serde::Serialize for ByteStr<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for ByteBuf {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_bytes(ByteBufVisitor)
    }
}

struct ByteBufVisitor;

impl serde::de::Visitor<'_> for ByteBufVisitor {
    type Value = ByteBuf;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a CBOR byte string (major type 2)")
    }

    fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
        Ok(ByteBuf(v.to_vec()))
    }

    fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
        Ok(ByteBuf(v))
    }
}

/// Read one element out of the array, naming it in the error when it is not there.
///
/// An absent element is reported as the field that went missing rather than as a
/// generic EOF, because the failure this catches is a document that was truncated or
/// written by a different element count — both of which are meaningless without the
/// index that was expected.
fn element<'de, A, T>(seq: &mut A, index: usize, name: &str) -> Result<T, A::Error>
where
    A: serde::de::SeqAccess<'de>,
    T: serde::Deserialize<'de>,
{
    seq.next_element::<T>()?
        .ok_or_else(|| serde::de::Error::custom(format!("element {index} ({name}) is missing")))
}

impl serde::Serialize for PayloadV2 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;

        let mut out = serializer.serialize_tuple(V2_ELEMENT_COUNT)?;
        out.serialize_element(&self.schema_version)?;
        out.serialize_element(&self.signal_type)?;
        out.serialize_element(&ByteStr(&self.origin_node_id))?;
        out.serialize_element(&ByteStr(&self.device_hash))?;
        out.serialize_element(&self.device_address.as_deref().map(ByteStr))?;
        out.serialize_element(&self.observed_at_node_local)?;
        out.serialize_element(&self.rssi)?;
        out.serialize_element(&self.tx_power)?;
        out.serialize_element(&self.adv_type)?;
        out.serialize_element(&self.location)?;
        out.serialize_element(&ByteStr(&self.signal_payload))?;
        out.serialize_element(&self.advertised_name)?;
        out.serialize_element(&self.observed_at)?;
        out.serialize_element(&self.alt_m)?;
        out.serialize_element(&self.accuracy_m)?;
        out.serialize_element(&self.location_source)?;
        out.serialize_element(&self.geo_cell_fine)?;
        out.serialize_element(&self.geo_cell_macro)?;
        out.end()
    }
}

impl<'de> serde::Deserialize<'de> for PayloadV2 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `deserialize_seq`, not `deserialize_struct`: ciborium maps the latter to a
        // CBOR map, and a keyed document claiming to be v2 is the confusion this
        // version is meant to make impossible.
        deserializer.deserialize_seq(PayloadV2Visitor)
    }
}

struct PayloadV2Visitor;

impl<'de> serde::de::Visitor<'de> for PayloadV2Visitor {
    type Value = PayloadV2;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "a CBOR array of {} elements (a version 2 signed payload)",
            V2_ELEMENT_COUNT
        )
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let payload = PayloadV2 {
            schema_version: element(&mut seq, 0, "schema_version")?,
            signal_type: element(&mut seq, 1, "signal_type")?,
            origin_node_id: element::<_, ByteBuf>(&mut seq, 2, "origin_node_id")?.0,
            device_hash: element::<_, ByteBuf>(&mut seq, 3, "device_hash")?.0,
            device_address: element::<_, Option<ByteBuf>>(&mut seq, 4, "device_address")
                .map(|value| value.map(ByteBuf::into_vec))?,
            observed_at_node_local: element(&mut seq, 5, "observed_at_node_local")?,
            rssi: element(&mut seq, 6, "rssi")?,
            tx_power: element(&mut seq, 7, "tx_power")?,
            adv_type: element(&mut seq, 8, "adv_type")?,
            location: element(&mut seq, 9, "location")?,
            signal_payload: element::<_, ByteBuf>(&mut seq, 10, "signal_payload")?.0,
            advertised_name: element(&mut seq, 11, "advertised_name")?,
            observed_at: element(&mut seq, 12, "observed_at")?,
            alt_m: element(&mut seq, 13, "alt_m")?,
            accuracy_m: element(&mut seq, 14, "accuracy_m")?,
            location_source: element(&mut seq, 15, "location_source")?,
            geo_cell_fine: element(&mut seq, 16, "geo_cell_fine")?,
            geo_cell_macro: element(&mut seq, 17, "geo_cell_macro")?,
        };

        // The head said how many elements there are; a document that carries more
        // than the version defines is not a payload with a spare field, it is a
        // document whose meaning is unknown.
        if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(format!(
                "a version 2 payload has {} elements, not more",
                V2_ELEMENT_COUNT
            )));
        }

        Ok(payload)
    }
}

impl ByteBuf {
    fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

impl PayloadV2 {
    /// Create a v2 payload builder.
    pub fn builder() -> PayloadV2Builder {
        PayloadV2Builder::new()
    }

    /// Get the signal type as a string representation.
    pub fn signal_type_str(&self) -> &'static str {
        signal_type_str(self.signal_type)
    }

    /// Attest to an occurrence, field by field, derived from the row itself.
    ///
    /// Deriving rather than assembling the two in parallel is the point: the payload
    /// cannot then disagree with the row it is attached to, because it is a function
    /// of it. The proof fields of `row` (`signed_payload`, `signature`) are not read
    /// and need not be set yet, so the node builds the row, derives the attestation,
    /// signs it, and writes the result back into the row.
    ///
    /// The H3 cells are re-derived from `row.location` through [`repo::geo`], the
    /// client-side implementation the server's generated columns are already checked
    /// against (`repo/tests/wire_types.rs` asserts h3-pg and `repo::geo` return the
    /// same cell for the same point).
    ///
    /// # Errors
    ///
    /// [`PayloadError::SignalPayload`] if the JSON column cannot be serialised, and
    /// [`PayloadError::Geo`] if the stored location is outside the range h3 accepts.
    pub fn from_occurrence(row: &Occurrence) -> Result<Self, PayloadError> {
        let signal_payload = super::encode::canonical_signal_payload(&row.signal_payload)?;

        let mut builder = PayloadV2::builder()
            .signal_type(signal_type_code(row.signal_type))
            .origin_node_id(&row.origin_node_id)
            .device_hash(&row.device_hash)
            .observed_at(row.observed_at)
            .observed_at_node_local(row.observed_at_node_local)
            .rssi(row.rssi)
            .location_source(row.location_source)
            .signal_payload(&signal_payload);

        if let Some(address) = row.device_address.as_deref() {
            builder = builder.device_address(address);
        }
        if let Some(power) = row.tx_power {
            builder = builder.tx_power(power);
        }
        if let Some(adv_type) = row.adv_type {
            builder = builder.adv_type(adv_type);
        }
        if let Some(point) = row.location.as_ref() {
            let (lat, lon) = (point.0.y(), point.0.x());
            builder = builder.location(lat, lon);
            let (fine, macro_cell) = signed_geo_cells(point)?;
            builder = builder.geo_cell_fine(fine).geo_cell_macro(macro_cell);
        }
        if let Some(alt_m) = row.alt_m {
            builder = builder.alt_m(alt_m);
        }
        if let Some(accuracy_m) = row.accuracy_m {
            builder = builder.accuracy_m(accuracy_m);
        }
        if let Some(name) = row.advertised_name.as_deref() {
            builder = builder.advertised_name(name);
        }

        Ok(builder.build())
    }
}

/// The two cells the generated columns will hold for `point`.
fn signed_geo_cells(point: &PostgisPoint) -> Result<(u64, u64), PayloadError> {
    // The geography column is (x = lon, y = lat); repo::geo takes them by name.
    let (lat, lon) = (point.0.y(), point.0.x());
    let cell = |result: Result<CellIndex, geo::GeoError>| {
        result
            .map(u64::from)
            .map_err(|source| PayloadError::Geo { lat, lon, source })
    };
    Ok((
        cell(geo::fine_cell(lat, lon))?,
        cell(geo::macro_cell(lat, lon))?,
    ))
}

/// The signal type code table shared by both payload versions.
///
/// Lives here rather than on `repo`'s `SignalType` because the code table belongs to
/// the signed format; the database has its own `signal_type` enum and needs no
/// knowledge of the numbers the wire uses.
fn signal_type_code(signal_type: SignalType) -> u8 {
    match signal_type {
        SignalType::Bluetooth => 0,
        SignalType::Wifi => 1,
        SignalType::Nfc => 2,
        SignalType::Zigbee => 3,
        SignalType::Lorawan => 4,
    }
}

/// The name a signal type code stands for.
fn signal_type_str(code: u8) -> &'static str {
    match code {
        0 => "bluetooth",
        1 => "wifi",
        2 => "nfc",
        3 => "zigbee",
        4 => "lorawan",
        _ => "unknown",
    }
}

/// The `adv_type` wire code table — signed payload v2 element 8.
///
/// The spec asks for a byte value here, and the spec is also where the numbers are
/// defined, not the database: the `adv_type` column is a PostgreSQL enum whose labels
/// are ordered by creation, not by meaning, so nothing on the server side assigns
/// `connectable_adv` the number 0. That is this table's job.
///
/// The mapping is total over [`AdvType::all()`] — an exhaustive `match`, so a new
/// variant is a compile error rather than a code silently reused — and
/// `repo/tests/wire_types.rs` pins `AdvType::all()` and `as_str()` against `pg_enum`,
/// which is what ties these numbers to the column. A code is only ever decoded through
/// [`adv_type_label`], and a code outside 0..=3 is refused on read.
fn adv_type_code(adv_type: AdvType) -> u8 {
    match adv_type {
        AdvType::ConnectableAdv => 0,
        AdvType::ScannableAdv => 1,
        AdvType::BroadcastAdv => 2,
        AdvType::ExtendedAdv => 3,
    }
}

/// The advertisement type a wire code stands for, or `None` for an unassigned code.
///
/// `pub(crate)` so the decode path in [`super::encode`] can refuse a code the table
/// does not define; nothing outside this module maps numbers to labels.
pub(crate) fn adv_type_label(code: u8) -> Option<&'static str> {
    match code {
        0 => Some("connectable_adv"),
        1 => Some("scannable_adv"),
        2 => Some("broadcast_adv"),
        3 => Some("extended_adv"),
        _ => None,
    }
}

/// The `location_source` wire code table — signed payload v2 element 15.
///
/// Same arrangement as [`adv_type_code`], including why the table lives here rather
/// than on the enum: the numbers belong to the signed format, and the column's own
/// enum has no numbers to give.
fn location_source_code(source: LocationSource) -> u8 {
    match source {
        LocationSource::NodeFixed => 0,
        LocationSource::NodeGps => 1,
        LocationSource::Interpolated => 2,
        LocationSource::AggregatorFixed => 3,
    }
}

/// The location source a wire code stands for, or `None` for an unassigned code.
pub(crate) fn location_source_label(code: u8) -> Option<&'static str> {
    match code {
        0 => Some("node_fixed"),
        1 => Some("node_gps"),
        2 => Some("interpolated"),
        3 => Some("aggregator_fixed"),
        _ => None,
    }
}

/// Builder for creating [`PayloadV1`] instances.
#[derive(Debug)]
pub struct PayloadV1Builder {
    payload: PayloadV1,
}

impl PayloadV1Builder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            payload: PayloadV1 {
                schema_version: VERSION_V1,
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
    ///
    /// Only worth calling to produce a v1 document explicitly; [`PayloadV2Builder`]
    /// is how a v2 document is built.
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

    /// Build the `PayloadV1`.
    pub fn build(self) -> PayloadV1 {
        self.payload
    }
}

impl Default for PayloadV1Builder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for creating [`PayloadV2`] instances.
///
/// The version is fixed at [`VERSION_V2`]: a builder that could relabel a v2 field
/// set as version 1 (or 3) would produce documents whose declared version lies
/// about what they cover.
#[derive(Debug)]
pub struct PayloadV2Builder {
    payload: PayloadV2,
}

impl PayloadV2Builder {
    /// Create a builder for a version 2 payload, with every field absent.
    pub fn new() -> Self {
        Self {
            payload: PayloadV2 {
                schema_version: VERSION_V2,
                signal_type: 0, // Bluetooth by default
                origin_node_id: Vec::new(),
                device_hash: Vec::new(),
                device_address: None,
                observed_at: String::new(),
                observed_at_node_local: String::new(),
                rssi: 0,
                tx_power: None,
                adv_type: None,
                location: None,
                alt_m: None,
                accuracy_m: None,
                geo_cell_fine: None,
                geo_cell_macro: None,
                signal_payload: Vec::new(),
                advertised_name: None,
                // The one element with no empty value to fall back on: the column is
                // `NOT NULL`, so 0 (`node_fixed`) is what an unset builder signs. Named
                // rather than hidden — the node always sets it (the position source or
                // the explicit no-fix label in `signed_occurrence`), and
                // `the_signature_names_every_column_the_node_authors` is what catches a
                // caller that forgot.
                location_source: location_source_code(LocationSource::NodeFixed),
            },
        }
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
    /// # Returns
    /// * `Ok(Self)` if valid signal type
    /// * `Err(String)` if unknown signal type
    pub fn signal_type_str(self, signal_type: &str) -> Result<Self, String> {
        match signal_type.to_lowercase().as_str() {
            "bluetooth" => Ok(self.signal_type(0)),
            "wifi" => Ok(self.signal_type(1)),
            "nfc" => Ok(self.signal_type(2)),
            "zigbee" => Ok(self.signal_type(3)),
            "lorawan" => Ok(self.signal_type(4)),
            other => Err(format!("Unknown signal type: {}", other)),
        }
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

    /// Set `observed_at`, the sync-corrected time, in the canonical spelling.
    pub fn observed_at(mut self, observed_at: DateTime<Utc>) -> Self {
        self.payload.observed_at = canonical_timestamp(observed_at);
        self
    }

    /// Set `observed_at_node_local`, the raw clock reading, in the canonical spelling.
    pub fn observed_at_node_local(mut self, observed_at_node_local: DateTime<Utc>) -> Self {
        self.payload.observed_at_node_local = canonical_timestamp(observed_at_node_local);
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

    /// Set the `adv_type` column's value, as the code the wire carries.
    ///
    /// Takes the enum rather than a number so that no caller can put an unassigned
    /// code inside a signature; [`adv_type_code`] is the only mapping.
    pub fn adv_type(mut self, adv_type: AdvType) -> Self {
        self.payload.adv_type = Some(adv_type_code(adv_type));
        self
    }

    /// Set the location (latitude, longitude).
    pub fn location(mut self, lat: f64, lon: f64) -> Self {
        self.payload.location = Some([lat, lon]);
        self
    }

    /// Set `alt_m` in metres.
    pub fn alt_m(mut self, alt_m: f32) -> Self {
        self.payload.alt_m = Some(alt_m);
        self
    }

    /// Set `accuracy_m` in metres.
    pub fn accuracy_m(mut self, accuracy_m: f32) -> Self {
        self.payload.accuracy_m = Some(accuracy_m);
        self
    }

    /// Set the `location_source` column's value, as the code the wire carries.
    pub fn location_source(mut self, source: LocationSource) -> Self {
        self.payload.location_source = location_source_code(source);
        self
    }

    /// Set the fine (resolution 9) H3 cell.
    pub fn geo_cell_fine(mut self, cell: u64) -> Self {
        self.payload.geo_cell_fine = Some(cell);
        self
    }

    /// Set the macro (resolution 6) H3 cell.
    pub fn geo_cell_macro(mut self, cell: u64) -> Self {
        self.payload.geo_cell_macro = Some(cell);
        self
    }

    /// Set the canonical signal-payload bytes.
    ///
    /// Callers holding the JSON column value go through
    /// [`encode::canonical_signal_payload`](super::encode::canonical_signal_payload)
    /// rather than serialising it themselves — the point of signing that column is
    /// that there is exactly one way to spell it.
    pub fn signal_payload(mut self, payload: &[u8]) -> Self {
        self.payload.signal_payload = payload.to_vec();
        self
    }

    /// Set the advertised name.
    pub fn advertised_name(mut self, name: &str) -> Self {
        self.payload.advertised_name = Some(name.to_string());
        self
    }

    /// Build the `PayloadV2`.
    pub fn build(self) -> PayloadV2 {
        self.payload
    }
}

impl Default for PayloadV2Builder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use repo::models::LocationSource;

    /// An instant from an RFC 3339 fixture, so the tests name the moment rather
    /// than an epoch count nobody can read.
    fn instant(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("fixture timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn v1_builder_still_declares_version_one() {
        let payload = PayloadV1::builder().build();
        assert_eq!(payload.schema_version, VERSION_V1);
    }

    #[test]
    fn v2_builder_declares_version_two_and_cannot_be_relabelled() {
        // There is deliberately no schema_version setter on this builder: a v2 field
        // set that declared itself version 1 would claim to cover less than it does.
        let payload = PayloadV2::builder().build();
        assert_eq!(payload.schema_version, VERSION_V2);
    }

    #[test]
    fn v2_carries_every_field_the_producer_sets() {
        let node_id = vec![0u8; 32];
        let device_hash = vec![1u8; 32];
        let address = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let observed = instant("2026-09-15T12:00:00Z");

        let payload = PayloadV2::builder()
            .signal_type(0)
            .origin_node_id(&node_id)
            .device_hash(&device_hash)
            .device_address(&address)
            .observed_at(observed)
            .observed_at_node_local(observed)
            .rssi(-67)
            .tx_power(4)
            .adv_type(AdvType::ConnectableAdv)
            .location(40.6892, -74.0445)
            .alt_m(12.5)
            .accuracy_m(4.0)
            .location_source(LocationSource::NodeGps)
            .geo_cell_fine(0x892a1072b5bffff)
            .geo_cell_macro(0x0892a1000000fff)
            .signal_payload(b"{\"ble\":{}}")
            .advertised_name("TestDevice")
            .build();

        assert_eq!(payload.origin_node_id, node_id);
        assert_eq!(payload.device_hash, device_hash);
        assert_eq!(payload.device_address, Some(address));
        assert_eq!(payload.rssi, -67);
        assert_eq!(payload.tx_power, Some(4));
        assert_eq!(
            payload.adv_type,
            Some(adv_type_code(AdvType::ConnectableAdv))
        );
        assert_eq!(
            payload.adv_type.and_then(adv_type_label),
            Some("connectable_adv")
        );
        assert_eq!(payload.location, Some([40.6892, -74.0445]));
        assert_eq!(payload.alt_m, Some(12.5));
        assert_eq!(payload.accuracy_m, Some(4.0));
        assert_eq!(
            payload.location_source,
            location_source_code(LocationSource::NodeGps)
        );
        assert_eq!(
            location_source_label(payload.location_source),
            Some("node_gps")
        );
        assert_eq!(payload.geo_cell_fine, Some(0x892a1072b5bffff));
        assert_eq!(payload.geo_cell_macro, Some(0x0892a1000000fff));
        assert_eq!(payload.signal_payload, br#"{"ble":{}}"#);
        assert_eq!(payload.advertised_name.as_deref(), Some("TestDevice"));
        assert_eq!(payload.signal_type_str(), "bluetooth");
    }

    #[test]
    fn version_two_columns_are_optional_where_the_columns_are_nullable() {
        // adv_type and tx_power have no source in the capture layer yet, and a node
        // with no fix signs no location. Absence is representable; the signature
        // says "this node recorded nothing here", which is not the same as the
        // column being unsigned.
        let payload = PayloadV2::builder().build();
        assert!(payload.tx_power.is_none());
        assert!(payload.adv_type.is_none());
        assert!(payload.location.is_none());
        assert!(payload.alt_m.is_none());
        assert!(payload.accuracy_m.is_none());
        assert!(payload.geo_cell_fine.is_none());
        assert!(payload.geo_cell_macro.is_none());
        assert!(payload.advertised_name.is_none());
        // `location_source` is the one NOT NULL column with no empty value: code 0 is
        // what an unset builder signs, so a caller that never set it still signs
        // `node_fixed`. `signed_occurrence` always sets it from the position source,
        // and `every_field_of_version_two_is_inside_the_bytes` in `encode.rs` is what
        // proves a forgotten setter would have changed the signature rather than
        // silently matched a default. `signal_payload` has an empty value and an unset
        // one is a bug in the caller, not a default that hides inside a signature.
        assert_eq!(
            payload.location_source,
            location_source_code(LocationSource::NodeFixed)
        );
        assert!(payload.signal_payload.is_empty());
    }

    #[test]
    fn covers_row_separates_the_versions_that_attest_to_different_amounts() {
        assert!(!CanonicalPayload::from(PayloadV1::builder().build()).covers_row());
        assert!(CanonicalPayload::from(PayloadV2::builder().build()).covers_row());
    }

    #[test]
    fn the_shared_accessors_read_both_versions() {
        let node_id = vec![7u8; 32];
        let v1 = CanonicalPayload::from(
            PayloadV1::builder()
                .origin_node_id(&node_id)
                .device_hash(&[8u8; 32])
                .device_address(&[0xAA, 0xBB])
                .observed_at_node_local("2026-09-15T12:00:00Z")
                .rssi(-50)
                .location(1.0, 2.0)
                .advertised_name("v1-name")
                .build(),
        );
        let v2 = CanonicalPayload::from(
            PayloadV2::builder()
                .origin_node_id(&node_id)
                .device_hash(&[8u8; 32])
                .device_address(&[0xAA, 0xBB])
                .observed_at(instant("2026-09-15T12:00:00.5Z"))
                .observed_at_node_local(instant("2026-09-15T12:00:00.5Z"))
                .rssi(-50)
                .location(1.0, 2.0)
                .advertised_name("v2-name")
                .build(),
        );

        for (payload, name) in [(&v1, "v1"), (&v2, "v2")] {
            assert_eq!(payload.origin_node_id(), node_id.as_slice(), "{name}");
            assert_eq!(payload.device_hash(), [8u8; 32].as_slice(), "{name}");
            assert_eq!(
                payload.device_address(),
                Some([0xAA, 0xBB].as_slice()),
                "{name}"
            );
            assert_eq!(payload.rssi(), -50, "{name}");
            assert_eq!(payload.location(), Some([1.0, 2.0]), "{name}");
            assert_eq!(payload.signal_type_str(), "bluetooth", "{name}");
        }

        assert_eq!(v1.version(), VERSION_V1);
        assert_eq!(v2.version(), VERSION_V2);
        // The column v1 leaves unsigned is the one the accessor has to answer None
        // for, rather than inventing a corrected time it never saw.
        assert_eq!(v1.observed_at(), None);
        assert_eq!(v2.observed_at(), Some("2026-09-15T12:00:00.500000+00:00"));
        assert_eq!(v1.advertised_name(), Some("v1-name"));
        assert_eq!(v2.advertised_name(), Some("v2-name"));
        assert_eq!(v1.signed_signal_payload(), None);
        assert_eq!(v2.signed_signal_payload(), Some(&[][..]));
    }

    #[test]
    fn canonical_timestamp_is_microseconds_and_truncates_rather_than_rounds() {
        // ...789 ns: the row keeps ...456 µs, it does not round up to 123457.
        let nanos = instant("2026-09-15T12:00:00.123456789Z");
        assert_eq!(
            canonical_timestamp(nanos),
            "2026-09-15T12:00:00.123456+00:00"
        );

        // Whole seconds still get six digits, so the spelling does not depend on how
        // many trailing digits the value happened to carry.
        assert_eq!(
            canonical_timestamp(instant("2026-09-15T12:00:00Z")),
            "2026-09-15T12:00:00.000000+00:00"
        );
    }

    #[test]
    fn a_signed_timestamp_can_be_read_back_off_the_instant_it_names() {
        let nanos = instant("2026-09-15T12:00:00.123456789Z");
        let signed = canonical_timestamp(nanos);

        let parsed = chrono::DateTime::parse_from_rfc3339(&signed)
            .expect("canonical timestamps parse")
            .with_timezone(&Utc);
        assert_eq!(parsed, truncate_to_micros(nanos));

        // What v1 signed is not what a reader of the column gets back — which is the
        // reason v2 does not spell timestamps that way.
        assert_ne!(nanos.to_rfc3339(), signed);
        assert!(nanos.to_rfc3339().ends_with(".123456789+00:00"));
    }

    #[test]
    fn truncate_to_micros_leaves_a_microsecond_value_alone() {
        let micros = instant("2026-09-15T12:00:00.000123Z");
        assert_eq!(truncate_to_micros(micros), micros);
        assert_eq!(
            truncate_to_micros(instant("2026-09-15T12:00:00.999999999Z")),
            instant("2026-09-15T12:00:00.999999Z")
        );
    }

    #[test]
    fn signal_type_codes_match_the_table_both_versions_share() {
        for (signal_type, code) in [
            (SignalType::Bluetooth, 0),
            (SignalType::Wifi, 1),
            (SignalType::Nfc, 2),
            (SignalType::Zigbee, 3),
            (SignalType::Lorawan, 4),
        ] {
            assert_eq!(signal_type_code(signal_type), code);
        }
        // A code nothing named is reported as unknown rather than mapped onto a
        // signal type that happens to be first in the table.
        assert_eq!(signal_type_str(99), "unknown");
    }

    #[test]
    fn v2_signal_type_str_reads_from_the_same_table_as_v1() {
        let payload = PayloadV2::builder()
            .signal_type_str("wifi")
            .unwrap()
            .build();
        assert_eq!(payload.signal_type, 1);
        assert_eq!(payload.signal_type_str(), "wifi");

        PayloadV2::builder().signal_type_str("morse").unwrap_err();
    }

    #[test]
    fn an_enum_column_gives_the_label_the_column_holds() {
        // The wire carries a code, not these strings, so they are what the code table
        // has to stay in step with: they are the spellings PostgreSQL stores in the
        // column, and `repo/tests/wire_types.rs` checks them against `pg_enum`.
        assert_eq!(LocationSource::NodeGps.as_str(), "node_gps");
        assert_eq!(LocationSource::AggregatorFixed.as_str(), "aggregator_fixed");
    }

    #[test]
    fn every_enum_variant_has_its_own_code_and_the_code_maps_back_to_the_column_label() {
        // Both directions, over every variant: a code assigned twice would make two
        // different column values attest identically, and a code that does not read
        // back as the label the column holds would make a verifier reject an honest
        // row — or accept a dishonest one.
        let mut seen = std::collections::HashSet::new();
        for &adv_type in AdvType::all() {
            let code = adv_type_code(adv_type);
            assert!(
                seen.insert(code),
                "code {code} assigned to two adv_type variants"
            );
            assert_eq!(adv_type_label(code), Some(adv_type.as_str()));
        }

        let mut seen = std::collections::HashSet::new();
        for &source in LocationSource::all() {
            let code = location_source_code(source);
            assert!(
                seen.insert(code),
                "code {code} assigned to two location_source variants"
            );
            assert_eq!(location_source_label(code), Some(source.as_str()));
        }
    }

    #[test]
    fn a_code_outside_the_table_reads_as_no_label_at_all() {
        // A future release that adds `adv_type = 4` must not have this build answer
        // with a guess; the decode path in `encode.rs` refuses such a payload rather
        // than storing a label no column can hold.
        assert_eq!(adv_type_label(4), None);
        assert_eq!(adv_type_label(255), None);
        assert_eq!(location_source_label(4), None);
        assert_eq!(location_source_label(255), None);
    }

    #[test]
    fn v1_builder_optional_field_toggles_still_work() {
        let payload = PayloadV1::builder()
            .device_address(&[1, 2])
            .no_device_address()
            .tx_power(3)
            .no_tx_power()
            .adv_type(1)
            .no_adv_type()
            .location(1.0, 2.0)
            .no_location()
            .signal_payload(b"x")
            .no_signal_payload()
            .advertised_name("n")
            .no_advertised_name()
            .build();

        assert!(payload.device_address.is_none());
        assert!(payload.tx_power.is_none());
        assert!(payload.adv_type.is_none());
        assert!(payload.location.is_none());
        assert!(payload.signal_payload.is_none());
        assert!(payload.advertised_name.is_none());
    }
}
