//! CBOR encoding and decoding for canonical payloads.
//!
//! One encoder per signed document, and one decoder that dispatches on the
//! document's own `schema_version`: a stored row says which shape it holds, and a
//! version this build cannot name is an error rather than a best guess. See
//! [`payload`](super::payload) for what each version covers and why v2 exists.
//!
//! # Deterministic encoding
//!
//! The same payload always produces identical bytes, which is what makes signing them
//! meaningful. For version 2 that is stronger than a property of this encoder: the
//! document is a positional array with no keys, so there is no key order for two
//! implementations to diverge over, and `ciborium`'s writer emits minimal integer heads
//! and the shortest float that round-trips bit-exactly — the two things
//! [RFC 8949 §4.2.1](https://datatracker.ietf.org/doc/html/rfc8949#section-4.2.1)
//! asks of an encoder. `version_two_bytes_are_pinned` checks the output against bytes
//! written from the layout rather than against this encoder's own output, and the
//! `the_encoder_obeys_*` tests walk the heads; together they are the golden vectors
//! [GAP_ANALYSIS B9](../../../GAP_ANALYSIS.md#81-blocking) called for.
//!
//! Version 1 is the exception, and stays one: it is a text-keyed map whose keys come
//! out in Rust field-declaration order, which is *deterministic under this encoder* but
//! not §4.2.1 canonical. Its bytes were signed by nodes that no longer run this code, so
//! the encoding is frozen — `the_v1_encoder_is_untouched_by_the_new_version` pins it
//! against a fixture captured before v2 existed.
//!
//! A verifier in either case consumes the stored `signed_payload` bytes verbatim, which
//! is all that is needed to check a signature. What it must not do is re-encode a row
//! and expect to reproduce a v1 document from a different implementation.
//!
//! # Example
//!
//! ```ignore
//! use app::provenance::{
//!     payload::PayloadV2,
//!     encode::{encode_payload, decode_payload},
//! };
//!
//! let payload = PayloadV2::builder().build();
//! let encoded = encode_payload(&payload.into())?;
//! let decoded = decode_payload(&encoded)?;
//! assert_eq!(decoded.version(), 2);
//! ```

use ciborium::{de::from_reader, ser::into_writer, Value};
use std::io::Cursor;

use super::payload::{
    adv_type_label, location_source_label, CanonicalPayload, PayloadV1, PayloadV2, VERSION_V1,
    VERSION_V2,
};

/// Error type for encoding/decoding operations.
#[derive(Debug, Clone, PartialEq)]
pub enum EncodeError {
    /// CBOR encoding failed.
    Encoding(String),

    /// CBOR decoding failed.
    Decoding(String),

    /// Invalid payload data.
    InvalidData(String),

    /// The document declares a version this build has no shape for.
    ///
    /// Distinct from [`EncodeError::Decoding`] on purpose: "this is a payload from
    /// the future" and "this is not a payload" lead to different operator actions,
    /// and the first one used to be silently accepted — a v3 document whose field
    /// names still lined up decoded cleanly as v1, attesting to less than it claimed.
    UnsupportedVersion(u16),

    /// The document's shape is not the shape its declared version specifies.
    ///
    /// A map that says it is version 2 and an array that says it is version 1 are both
    /// documents that would decode against the wrong field set if the reader picked a
    /// shape by looking at the fields instead of trusting the number. Neither is
    /// trusted: the number picks the shape, and a document that disagrees with itself
    /// is refused.
    MismatchedShape {
        /// The version the document declared.
        version: u16,
        /// The shape that version requires.
        expected: &'static str,
    },

    /// The buffer holds a complete document with bytes left over after it.
    ///
    /// Those bytes are not part of anything that was signed.
    TrailingBytes {
        /// How many bytes the document occupied.
        document: u64,
        /// How many bytes were offered.
        offered: usize,
    },

    /// An element whose meaning is a code carries one no table assigns.
    UnknownCode {
        /// The element's index in the array.
        element: usize,
        /// The column the element attests to.
        field: &'static str,
        /// The code the document carried.
        code: u8,
    },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Encoding(msg) => write!(f, "Encoding error: {}", msg),
            EncodeError::Decoding(msg) => write!(f, "Decoding error: {}", msg),
            EncodeError::InvalidData(msg) => write!(f, "Invalid data: {}", msg),
            EncodeError::UnsupportedVersion(v) => {
                write!(f, "Unsupported payload schema_version: {}", v)
            }
            EncodeError::MismatchedShape { version, expected } => write!(
                f,
                "a version {} signed payload is {}; this document is not",
                version, expected
            ),
            EncodeError::TrailingBytes { document, offered } => write!(
                f,
                "a signed payload is one CBOR document: {} of the {} bytes offered belong to it",
                document, offered
            ),
            EncodeError::UnknownCode {
                element,
                field,
                code,
            } => write!(
                f,
                "element {} ({}) carries code {}, which no version defines",
                element, field, code
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<ciborium::ser::Error<std::io::Error>> for EncodeError {
    fn from(err: ciborium::ser::Error<std::io::Error>) -> Self {
        EncodeError::Encoding(err.to_string())
    }
}

impl From<ciborium::de::Error<std::io::Error>> for EncodeError {
    fn from(err: ciborium::de::Error<std::io::Error>) -> Self {
        EncodeError::Decoding(err.to_string())
    }
}

/// Result type for encoding operations.
pub type Result<T> = std::result::Result<T, EncodeError>;

/// Encode a canonical payload to CBOR bytes, whatever version it is.
///
/// # Arguments
///
/// * `payload` - The canonical payload to encode
///
/// # Returns
///
/// * `Ok(Vec<u8>)` - The encoded CBOR bytes
/// * `Err(EncodeError)` - If encoding fails
pub fn encode_payload(payload: &CanonicalPayload) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    match payload {
        CanonicalPayload::V1(payload) => into_writer(payload, &mut buffer)?,
        CanonicalPayload::V2(payload) => into_writer(payload, &mut buffer)?,
    }
    Ok(buffer)
}

/// The `schema_version` a signed document declares, without decoding its fields.
///
/// The version has to be read before the shape is known, which is the whole job of
/// this function: it is what lets a verifier tell "a payload I can check" from "a
/// payload from a newer node" instead of failing on a missing field or, worse,
/// succeeding against the wrong field set.
///
/// Where it is read from follows the shape, since the two shapes spell it
/// differently: a version 1 document is a map and says it under the `schema_version`
/// key, a version 2 document is an array and says it in element 0. Reading both is
/// what allows a keyed document claiming to be v2 — or a positional one claiming to
/// be v1 — to be reported as what it is ([`decode_payload`]) rather than decoded
/// against whichever shape happens to be reachable.
pub fn payload_version(data: &[u8]) -> Result<u16> {
    let (document, _) = single_document(data)?;
    declared_version(&document)
}

/// The version a parsed CBOR document declares, at the position its shape puts it.
fn declared_version(document: &Value) -> Result<u16> {
    let claimed = match document {
        Value::Map(fields) => fields
            .iter()
            .find(|(key, _)| matches!(key, Value::Text(name) if name == "schema_version"))
            .map(|(_, value)| value),
        Value::Array(elements) => elements.first(),
        _ => None,
    };

    let claimed = claimed.ok_or_else(|| match document {
        Value::Map(_) => {
            EncodeError::InvalidData("signed payload declares no schema_version".to_string())
        }
        Value::Array(_) => {
            EncodeError::InvalidData("signed payload array has no elements".to_string())
        }
        _ => EncodeError::InvalidData(
            "signed payload is neither a CBOR map nor a CBOR array".to_string(),
        ),
    })?;

    let declared = claimed
        .as_integer()
        .ok_or_else(|| EncodeError::InvalidData("schema_version is not an integer".to_string()))?;
    u16::try_from(declared).map_err(|_| {
        EncodeError::InvalidData(format!(
            "schema_version {} is outside the range a version number can take",
            i128::from(declared)
        ))
    })
}

/// Read the single CBOR document a signed payload is, and report how much of the
/// buffer it occupied.
///
/// CBOR does not oblige a reader to notice what follows the item it read, and ciborium
/// does not: hand it a document with bytes behind it and it returns the document as
/// though the rest were not there. For a stream that is correct. For a signature it is
/// not — whatever follows the document is bytes nobody signed, and a verifier that
/// ignores them has accepted a record whose contents are partly unsigned.
fn single_document(data: &[u8]) -> Result<(Value, u64)> {
    let mut cursor = Cursor::new(data);
    let document: Value = from_reader(&mut cursor)?;
    let consumed = cursor.position();

    if consumed != data.len() as u64 {
        return Err(EncodeError::TrailingBytes {
            document: consumed,
            offered: data.len(),
        });
    }

    Ok((document, consumed))
}

/// The shape a version's document has to be, phrased as the rejection that a document
/// of some other shape earns.
fn expected_shape(version: u16) -> &'static str {
    match version {
        VERSION_V1 => "a CBOR map with text keys",
        _ => "a positional CBOR array",
    }
}

/// Decode CBOR bytes to the payload they hold, in whatever version they declare.
///
/// # Arguments
///
/// * `data` - The CBOR bytes to decode
///
/// # Returns
///
/// * `Ok(CanonicalPayload)` - The decoded payload, as `V1` or `V2`
/// * `Err(EncodeError::UnsupportedVersion)` - For a version with no known shape
/// * `Err(EncodeError::MismatchedShape)` - For a document whose shape belongs to
///   another version than the one it declares
/// * `Err(EncodeError::TrailingBytes)` - For a buffer that carries anything after the
///   document
/// * `Err(EncodeError::UnknownCode)` - For an enum element outside its code table
/// * `Err(EncodeError)` - If the bytes are not a payload at all
///
/// Each version is decoded strictly. A document that declares version 1 but carries a
/// field from a later shape is rejected rather than read as a v1 document that quietly
/// omits the extra field; a v2 array with 17 or 19 elements is rejected rather than
/// read as a payload with an element missing or spare; and a code that no table assigns
/// is rejected rather than stored as a row no column can hold.
///
/// Strictness about *shape* is not strictness about encoding. A decoder accepts any
/// well-formed CBOR of the right shape, because RFC 8949 §4.2.1 constrains encoders,
/// not readers; what it will not do is accept a document that misrepresents which
/// version it is.
pub fn decode_payload(data: &[u8]) -> Result<CanonicalPayload> {
    let (document, _) = single_document(data)?;
    let version = declared_version(&document)?;

    match (version, &document) {
        (VERSION_V1, Value::Map(_)) => {
            let payload: PayloadV1 = from_reader(Cursor::new(data))?;
            Ok(CanonicalPayload::V1(payload))
        }
        (VERSION_V2, Value::Array(_)) => {
            let payload: PayloadV2 = from_reader(Cursor::new(data))?;
            check_codes(&payload)?;
            Ok(CanonicalPayload::V2(payload))
        }
        (VERSION_V1 | VERSION_V2, _) => Err(EncodeError::MismatchedShape {
            version,
            expected: expected_shape(version),
        }),
        (other, _) => Err(EncodeError::UnsupportedVersion(other)),
    }
}

/// Refuse a version 2 document whose enum elements carry codes no table assigns.
///
/// Such a document can be perfectly signed and still be unusable: the reader has no
/// column value to put the code in, and inventing one — or letting the label accessor
/// answer `None` and moving on — is how a forged element turns into a stored row. The
/// tables are small and closed, so refusing costs nothing.
fn check_codes(payload: &PayloadV2) -> Result<()> {
    if let Some(code) = payload
        .adv_type
        .filter(|code| adv_type_label(*code).is_none())
    {
        return Err(EncodeError::UnknownCode {
            element: 8,
            field: "adv_type",
            code,
        });
    }

    if location_source_label(payload.location_source).is_none() {
        return Err(EncodeError::UnknownCode {
            element: 15,
            field: "location_source",
            code: payload.location_source,
        });
    }

    Ok(())
}

/// The signed byte form of a JSON signal payload.
///
/// `signal_payload` is a `JSONB` column, and JSON is a text format with more than one
/// way to spell a value — so signing "the JSON" needs a rule, not a shrug. The rule
/// is `serde_json`'s compact serialisation, with object keys in `serde_json::Map`
/// order and no insignificant whitespace.
///
/// It is chosen for one property above all: it is reproducible from the stored column.
/// JSONB reorders object keys and collapses whitespace on the way in, and neither
/// survives this form, so a reader that pulls the column back out and re-serialises it
/// gets the bytes that were signed. Measured against a live server, that holds for the
/// cases a decimal store could break on — `1e30`, a 30-digit integer, a decimal that
/// needs 17 digits to round-trip — because both spellings parse back to the same value
/// (`repo/tests/wire_types.rs::a_jsonb_column_serialises_the_same_after_the_round_trip`).
///
/// What a reader must not do is compare the column's **text** against the signed bytes.
/// JSONB prints `{"a": 1}`, with a space after the colon and its own notation for
/// numbers; the signature is over compact bytes. Parse the value, then re-serialise it.
/// A verifier whose JSON keeps number text with more precision than `serde_json` does —
/// an arbitrary-precision parser, say — is the one that will disagree here, which is a
/// conformance question for the JSON encoder, not an integrity one: the v2 CBOR
/// document that carries these bytes is core-deterministic.
///
/// A `null` payload value encodes as the four bytes `null`, distinct from an empty
/// object `{}`: the first says the node recorded a nothing, the second that it recorded
/// nothing signal-specific.
///
/// ```ignore
/// let left = serde_json::json!({"ble": {"adv_type": "connectable_adv"}, "rssi": -67});
/// // Key order and whitespace are not part of the value, so they are not part of the
/// // signature either.
/// let right: serde_json::Value =
///     serde_json::from_str(r#"{ "rssi" : -67, "ble" : {"adv_type":"connectable_adv"} }"#).unwrap();
/// assert_eq!(canonical_signal_payload(&left).unwrap(), canonical_signal_payload(&right).unwrap());
/// ```
pub fn canonical_signal_payload(
    value: &serde_json::Value,
) -> std::result::Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(value)
}

/// Verify that encoding is deterministic.
///
/// This function encodes the same payload multiple times and verifies
/// that all encodings produce identical bytes.
///
/// It can only ever catch a serializer that is unstable within one build. It cannot
/// catch the encoder drifting from the specification, and it cannot catch a field
/// reorder — which changes every signature while staying perfectly deterministic.
/// The byte-level fixtures in this module's tests are what catch those.
///
/// # Arguments
///
/// * `payload` - The payload to test
/// * `iterations` - Number of times to encode (default: 100)
///
/// # Returns
///
/// * `Ok(true)` - All encodings are identical
/// * `Ok(false)` - Encodings differ (determinism violation!)
/// * `Err(EncodeError)` - If encoding fails
pub fn verify_determinism(payload: &CanonicalPayload, iterations: usize) -> Result<bool> {
    if iterations == 0 {
        return Ok(true);
    }

    let first_encoding = encode_payload(payload)?;

    for _ in 1..iterations {
        let encoding = encode_payload(payload)?;
        if encoding != first_encoding {
            return Ok(false);
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::payload::{CURRENT_VERSION, V2_ELEMENT_COUNT};
    use ed25519_dalek::{ed25519::signature::Signer, SigningKey};
    use rand::thread_rng;
    use repo::models::enums::{AdvType, LocationSource};

    /// A CBOR head, as the conformance tests read it off the wire.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Head {
        major: u8,
        info: u8,
        /// The head's argument: the value itself for major types 0 and 1, the length
        /// for 2, 3, 4 and 5, and the raw bits for a float head.
        argument: u64,
    }

    impl Head {
        /// True for the half-, single- and double-precision float heads of major type 7.
        fn is_float(&self) -> bool {
            self.major == 7 && (25..=27).contains(&self.info)
        }
    }

    /// Every head in a document, in wire order, outermost first.
    ///
    /// Deliberately a second reader rather than a use of ciborium: the claim under test
    /// is that the encoder emitted the encoding RFC 8949 §4.2.1 asks for, and a checker
    /// built on the same library would only be re-reading that library's own decisions.
    /// Each head is checked as it is read — no indefinite lengths, no reserved additional
    /// info, and no integer argument wider than its value needs — so running the walk at
    /// all is the assertion, and the returned heads are for the tests that want to look
    /// at types rather than at rule conformance.
    fn heads(data: &[u8]) -> Vec<Head> {
        let mut cursor = 0usize;
        let mut seen = Vec::new();

        while cursor < data.len() {
            read_item(data, &mut cursor, &mut seen);
        }

        seen
    }

    /// The heads of a document's top-level array elements, in element order.
    fn element_heads(data: &[u8]) -> Vec<Head> {
        let mut cursor = 0usize;
        let array = read_head(data, &mut cursor);
        assert_eq!(array.major, 4, "a signed payload is a CBOR array");

        (0..array.argument)
            .map(|_| {
                let mut seen = Vec::new();
                read_item(data, &mut cursor, &mut seen);
                seen.into_iter()
                    .next()
                    .expect("an element always has a head")
            })
            .collect()
    }

    fn read_item(data: &[u8], cursor: &mut usize, seen: &mut Vec<Head>) {
        let head = read_head(data, cursor);
        seen.push(head.clone());

        match head.major {
            2 | 3 => *cursor += head.argument as usize,
            4 => {
                for _ in 0..head.argument {
                    read_item(data, cursor, seen);
                }
            }
            // A map is something a signed payload must never contain
            // (`the_encoder_puts_no_maps_anywhere_in_a_payload` reports it), but the
            // walk still has to step over its keys and values to stay aligned with the
            // bytes it is reading.
            5 => {
                for _ in 0..head.argument {
                    read_item(data, cursor, seen);
                    read_item(data, cursor, seen);
                }
            }
            _ => {}
        }
    }

    fn read_head(data: &[u8], cursor: &mut usize) -> Head {
        let &byte = data
            .get(*cursor)
            .expect("the walk ran past the end of the document");
        *cursor += 1;
        let major = byte >> 5;
        let info = byte & 0x1f;

        assert_ne!(info, 31, "indefinite-length items are forbidden by §4.2.1");
        assert!(
            info < 28,
            "additional info {info} is reserved and must not be emitted"
        );

        // A float's argument is its payload rather than a length, so the rule about the
        // shortest integer form says nothing about it. Which float width is correct is
        // checked against the value, separately.
        if major == 7 && (25..=27).contains(&info) {
            return Head {
                major,
                info,
                argument: take_bytes(data, cursor, 2 << (info - 25)),
            };
        }

        let argument = match info {
            0..=23 => u64::from(info),
            24 => {
                let value = take_bytes(data, cursor, 1);
                assert!(value >= 24, "a value below 24 must use the head itself");
                value
            }
            25 => {
                let value = take_bytes(data, cursor, 2);
                assert!(
                    value > 0xFF,
                    "a value that fits in one byte must not use two"
                );
                value
            }
            26 => {
                let value = take_bytes(data, cursor, 4);
                assert!(
                    value > 0xFFFF,
                    "a value that fits in two bytes must not use four"
                );
                value
            }
            _ => {
                let value = take_bytes(data, cursor, 8);
                assert!(
                    value > 0xFFFF_FFFF,
                    "a value that fits in four bytes must not use eight"
                );
                value
            }
        };

        Head {
            major,
            info,
            argument,
        }
    }

    fn take_bytes(data: &[u8], cursor: &mut usize, count: usize) -> u64 {
        let window = data
            .get(*cursor..*cursor + count)
            .expect("the walk ran past the end of the document");
        *cursor += count;

        let mut value: u64 = 0;
        for &byte in window {
            value = (value << 8) | u64::from(byte);
        }
        value
    }

    /// The pinned v2 document, as the elements the layout names.
    ///
    /// Reading the fixture back as a `Value` gives the tests something to alter, which
    /// is how the strictness below is checked without writing a hand-rolled encoder for
    /// each malformed case.
    fn fixture_elements() -> Vec<Value> {
        let bytes = hex::decode(V2_FIXTURE_HEX).expect("fixture is hex");
        match from_reader::<Value, _>(Cursor::new(&bytes[..])).expect("fixture is CBOR") {
            Value::Array(elements) => elements,
            other => panic!("the v2 fixture is an array, got {other:?}"),
        }
    }

    /// Encode a document of arbitrary elements, for the cases no builder can produce.
    fn array_of(elements: &[Value]) -> Vec<u8> {
        let mut buffer = Vec::new();
        into_writer(&Value::Array(elements.to_vec()), &mut buffer).unwrap();
        buffer
    }

    /// The v1 document, byte for byte, as the pre-v2 encoder produced it.
    ///
    /// Captured before this change and checked in so it cannot be regenerated by the
    /// code under test. It is the evidence for the two claims that matter about v1: that
    /// a v1 document still decodes, and that adding v2 did not disturb a single byte of
    /// the v1 encoding. Its shape is a map of twelve pairs in declaration order, and
    /// `signal_payload` is a CBOR **array of integers** rather than a byte string
    /// because that is what serde made of a `Vec<u8>` — the v1 spelling is frozen
    /// whether or not it is the one the spec would choose today.
    const V1_FIXTURE_HEX: &str = "ac6e736368656d615f76657273696f6e016b7369676e616c5f74797065006e6f726967696e5f6e6f64655f6964982000000000000000000000000000000000000000000000000000000000000000006b6465766963655f68617368982001010101010101010101010101010101010101010101010101010101010101016e6465766963655f616464726573738618aa18bb18cc18dd18ee18ff766f627365727665645f61745f6e6f64655f6c6f63616c74323032362d30382d31355431323a30303a30305a647273736938426874785f706f77657200686164765f7479706500686c6f636174696f6e82fb40445837b4a2339cfbc05282d916872b026e7369676e616c5f7061796c6f61648c18741865187318741820187018611879186c186f186118646f616476657274697365645f6e616d656a54657374446576696365";

    /// The v2 document for [`full_v2`], written out from the layout table in
    /// `payload`'s docs rather than captured from this encoder.
    ///
    /// Every element is what the layout says it is, in the order the layout gives: the
    /// `92` head is a definite-length array of eighteen elements, the ids and the
    /// advertisement are byte strings (`58`/`46`) rather than arrays of integers, the
    /// two `REAL` columns are half-precision because 12.5 and 4.0 need nothing wider,
    /// and the two enum elements are the codes their tables assign (`00` for
    /// `connectable_adv`, `01` for `node_gps`).
    ///
    /// This is the vector that makes a field reorder, a widened float, or a byte field
    /// silently reverting to an integer array an expensive change rather than a quiet
    /// one: every signature written from here on differs from every signature written
    /// before it.
    const V2_FIXTURE_HEX: &str = "920200582000000000000000000000000000000000000000000000000000000000000000005820010101010101010101010101010101010101010101010101010101010101010146aabbccddeeff7820323032362d30382d31355431323a30303a30302e3132333939392b30303a30303842040082fb40445837b4a2339cfbc05282d916872b0258267b22626c65223a7b226164765f74797065223a22636f6e6e65637461626c655f616476227d7d6a546573744465766963657820323032362d30382d31355431323a30303a30302e3132333435362b30303a3030f94a40f94400011b0892a1072b5bffff1b00892a1000000fff";

    fn v1_fixture() -> PayloadV1 {
        PayloadV1::builder()
            .schema_version(1)
            .signal_type(0)
            .origin_node_id(&[0u8; 32])
            .device_hash(&[1u8; 32])
            .device_address(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .tx_power(0)
            .adv_type(0)
            .location(40.6892, -74.0445)
            .signal_payload(b"test payload")
            .advertised_name("TestDevice")
            .build()
    }

    /// A v2 payload with every field set, so a fixture that leaves a field out cannot
    /// hide a field that fails to encode.
    fn full_v2() -> PayloadV2 {
        PayloadV2::builder()
            .signal_type(0)
            .origin_node_id(&[0u8; 32])
            .device_hash(&[1u8; 32])
            .device_address(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
            .observed_at(
                chrono::DateTime::parse_from_rfc3339("2026-08-15T12:00:00.123456Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            )
            .observed_at_node_local(
                chrono::DateTime::parse_from_rfc3339("2026-08-15T12:00:00.123999Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            )
            .rssi(-67)
            .tx_power(4)
            .adv_type(AdvType::ConnectableAdv)
            .location(40.6892, -74.0445)
            .alt_m(12.5)
            .accuracy_m(4.0)
            .location_source(LocationSource::NodeGps)
            .geo_cell_fine(0x892a1072b5bffff)
            .geo_cell_macro(0x0892a1000000fff)
            .signal_payload(br#"{"ble":{"adv_type":"connectable_adv"}}"#)
            .advertised_name("TestDevice")
            .build()
    }

    /// A document with an arbitrary `schema_version`, to stand in for a node running
    /// a format this build has never seen.
    fn document_declaring(version: u64) -> Vec<u8> {
        let document = Value::Map(vec![(
            Value::Text("schema_version".to_string()),
            Value::Integer(version.into()),
        )]);
        let mut buffer = Vec::new();
        into_writer(&document, &mut buffer).unwrap();
        buffer
    }

    // ==================== version 1 stays readable ====================

    #[test]
    fn a_version_one_document_from_before_the_change_still_decodes() {
        let bytes = hex::decode(V1_FIXTURE_HEX).expect("fixture is hex");

        let decoded = decode_payload(&bytes).expect("v1 bytes must remain decodable");

        assert_eq!(decoded.version(), VERSION_V1);
        // And it says so, rather than being handed over as if it attested to the row.
        assert!(!decoded.covers_row());
        assert_eq!(decoded.rssi(), -67);
        assert_eq!(decoded.location(), Some([40.6892, -74.0445]));
        assert_eq!(decoded.advertised_name(), Some("TestDevice"));
        // The two columns v1 leaves unsigned, in the document that proves it: the
        // builder can set them, the node never did.
        assert_eq!(decoded.signed_signal_payload(), Some(&b"test payload"[..]));

        let CanonicalPayload::V1(payload) = decoded else {
            panic!("a version 1 document decodes as V1, got {decoded:?}");
        };
        assert_eq!(payload, v1_fixture());
    }

    #[test]
    fn the_v1_encoder_is_untouched_by_the_new_version() {
        // Byte equality against a fixture captured before v2 existed: the signed bytes
        // of every row already stored have to come out the same, or this change would
        // have invalidated them.
        let bytes = encode_payload(&CanonicalPayload::V1(v1_fixture())).unwrap();
        assert_eq!(hex::encode(&bytes), V1_FIXTURE_HEX);
    }

    #[test]
    fn a_version_one_signature_still_verifies_over_its_stored_bytes() {
        // What an auditor of an old row does: take the stored bytes, take the origin
        // node's key, check. Nothing about v2 is needed, and nothing about v2 changes
        // the answer.
        use crate::provenance::verify::verify_signature;

        let key = SigningKey::generate(&mut thread_rng());
        let bytes = hex::decode(V1_FIXTURE_HEX).unwrap();
        let signature = key.sign(&bytes);

        let decoded = decode_payload(&bytes).unwrap();
        assert_eq!(
            encode_payload(&decoded).unwrap(),
            bytes,
            "re-encoding a decoded v1 document reproduces it byte for byte"
        );
        verify_signature(&key.verifying_key(), &bytes, &signature)
            .expect("the stored v1 bytes are still the bytes that were signed");
    }

    // ==================== version 2 ====================

    #[test]
    fn a_version_two_document_round_trips_every_field() {
        let payload = full_v2();
        let encoded = encode_payload(&CanonicalPayload::V2(payload.clone())).unwrap();
        let decoded = decode_payload(&encoded).unwrap();

        let CanonicalPayload::V2(read_back) = &decoded else {
            panic!("expected v2, got {decoded:?}");
        };
        assert_eq!(read_back, &payload);
        assert!(decoded.covers_row());
    }

    #[test]
    fn a_version_two_document_with_every_optional_field_absent_round_trips() {
        let payload = PayloadV2::builder()
            .origin_node_id(&[3u8; 32])
            .device_hash(&[4u8; 32])
            .observed_at(chrono::Utc::now())
            .observed_at_node_local(chrono::Utc::now())
            .rssi(-90)
            .location_source(LocationSource::NodeGps)
            .signal_payload(b"{}")
            .build();

        let encoded = encode_payload(&CanonicalPayload::V2(payload.clone())).unwrap();
        let decoded = decode_payload(&encoded).unwrap();

        let CanonicalPayload::V2(read_back) = decoded else {
            panic!("expected v2, got {decoded:?}");
        };
        assert_eq!(read_back, payload);
        assert!(read_back.tx_power.is_none());
        assert!(read_back.adv_type.is_none());
        assert!(read_back.location.is_none());
        assert!(read_back.geo_cell_fine.is_none());
    }

    #[test]
    fn version_two_encoding_is_deterministic() {
        let payload = CanonicalPayload::V2(full_v2());
        assert!(verify_determinism(&payload, 100).unwrap());
    }

    #[test]
    fn version_two_bytes_are_pinned() {
        // A byte-level pin, not a round-trip: round-tripping proves the encoder and the
        // decoder agree with each other, which they can go on doing while the format
        // drifts. These bytes were written from the layout table in `payload`'s docs, so
        // a refactor that reorders two elements, widens a float, or lets a byte field go
        // back to being an array of integers fails here — and every signature written
        // from then on differs from every signature written before, which is what makes
        // the change expensive rather than quiet.
        let encoded = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();

        assert_eq!(hex::encode(&encoded), V2_FIXTURE_HEX);
        // The pin is only as good as the fixture, so the two independent readings of it
        // have to agree: these are the encoder's bytes, and they are the fixture.
        assert_eq!(
            decode_payload(&encoded).map(|payload| payload.version()),
            Ok(VERSION_V2)
        );
    }

    // ==================== RFC 8949 §4.2.1 conformance ====================

    #[test]
    fn a_version_two_document_is_an_array_of_the_declared_number_of_elements() {
        let encoded = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();

        assert_eq!(
            encoded[0],
            0x80 | V2_ELEMENT_COUNT as u8,
            "CBOR array head: {} elements",
            V2_ELEMENT_COUNT
        );
        assert_eq!(element_heads(&encoded).len(), V2_ELEMENT_COUNT);
    }

    #[test]
    fn every_element_is_the_type_the_layout_names() {
        // The element order and its types, read off the wire: the twelve elements the
        // specification indexes, then the six appended for row coverage. A reordered
        // struct, or a field whose type changed underneath the format, changes this list.
        let encoded = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();
        let majors: Vec<u8> = element_heads(&encoded)
            .iter()
            .map(|head| head.major)
            .collect();

        assert_eq!(
            majors,
            vec![
                0, //  0 schema_version: uint
                0, //  1 signal_type: uint
                2, //  2 origin_node_id: bytes
                2, //  3 device_hash: bytes
                2, //  4 device_address: bytes
                3, //  5 observed_at_node_local: text
                1, //  6 rssi: negative int
                0, //  7 tx_power: uint
                0, //  8 adv_type: uint code
                4, //  9 location: [lat, lon]
                2, // 10 signal_payload: bytes
                3, // 11 advertised_name: text
                3, // 12 observed_at: text
                7, // 13 alt_m: float
                7, // 14 accuracy_m: float
                0, // 15 location_source: uint code
                0, // 16 geo_cell_fine: uint
                0, // 17 geo_cell_macro: uint
            ]
        );
    }

    #[test]
    fn the_binary_fields_are_byte_strings_not_arrays_of_integers() {
        // The ambiguity B9 exists to remove: 32 zero bytes as `58 20 00...` and as
        // `98 20 00 00 ...` are different documents with different signatures, and a
        // decoder that accepted both would be signing something whose spelling depends on
        // which serde impl happened to run. Major type 2 is the only spelling, and
        // `a_byte_field_written_as_an_array_of_integers_is_refused` is the other half.
        let encoded = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();
        let heads = element_heads(&encoded);

        assert_eq!((heads[2].major, heads[2].argument), (2, 32));
        assert_eq!((heads[3].major, heads[3].argument), (2, 32));
        assert_eq!((heads[4].major, heads[4].argument), (2, 6));
        assert_eq!((heads[10].major, heads[10].argument), (2, 38));
    }

    #[test]
    fn the_encoder_puts_no_maps_anywhere_in_a_payload() {
        // The specification's own recommendation, and the reason the document is
        // core-deterministic: with no keys there is no key order to disagree about.
        let encoded = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();

        assert!(
            !heads(&encoded).iter().any(|head| head.major == 5),
            "a signed payload contains no CBOR map"
        );
        // No tags either: a tagged value would be a second way to spell a type the
        // layout already names.
        assert!(!heads(&encoded).iter().any(|head| head.major == 6));
    }

    #[test]
    fn floats_use_the_shortest_form_that_keeps_the_value() {
        // 12.5 and 4.0 are exact in binary16; the coordinates are not exact in anything
        // narrower than binary64. §4.2.1 asks for the shortest form that round-trips, so
        // anything else here means the encoder widened a value the column could hold in
        // fewer bytes — which changes the signature without changing the number.
        let encoded = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();
        let walked = heads(&encoded);
        let floats: Vec<&Head> = walked.iter().filter(|head| head.is_float()).collect();

        let infos: Vec<u8> = floats.iter().map(|head| head.info).collect();
        assert_eq!(
            infos,
            vec![
                27, //  40.6892, the latitude inside location
                27, // -74.0445, the longitude
                25, //  12.5 alt_m, exact in half precision
                25, //  4.0 accuracy_m, exact in half precision
            ]
        );

        // And the narrow forms still name the numbers they came from. `f16` is not a
        // stable Rust type, so the half is read back by hand rather than by borrowing
        // another implementation of the same arithmetic.
        assert_eq!(half_to_f32(floats[2].argument as u16), 12.5);
        assert_eq!(half_to_f32(floats[3].argument as u16), 4.0);
    }

    /// IEEE 754 binary16 to f32, for reading back a narrowed float off the wire.
    fn half_to_f32(bits: u16) -> f32 {
        let sign = u32::from(bits >> 15) << 31;
        let exponent = u32::from((bits >> 10) & 0x1f);
        let fraction = u32::from(bits & 0x3ff);

        let reconstructed = match exponent {
            0 => 0, // zero and subnormals, neither of which this format needs
            0x1f => 0x7f80_0000 | (fraction << 13),
            _ => ((exponent + 112) << 23) | (fraction << 13),
        };

        f32::from_bits(sign | reconstructed)
    }

    #[test]
    fn an_empty_version_two_document_is_still_eighteen_elements() {
        // Nothing is ever omitted from a positional array: an absent column is CBOR
        // null, which is what keeps element 15 at index 15 in every document.
        let encoded = encode_payload(&CanonicalPayload::V2(PayloadV2::builder().build())).unwrap();
        let heads = element_heads(&encoded);

        assert_eq!(heads.len(), V2_ELEMENT_COUNT);
        // 0xf6 is major type 7, additional info 22: null.
        let nulls: Vec<usize> = heads
            .iter()
            .enumerate()
            .filter(|(_, head)| head.major == 7 && head.info == 22)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            nulls,
            vec![4, 7, 8, 9, 11, 13, 14, 16, 17],
            "exactly the nullable elements are null, and the rest carry values"
        );
    }

    // ==================== the decoder is as strict as the encoder ====================

    #[test]
    fn a_version_two_array_with_the_wrong_number_of_elements_is_refused() {
        // An element is identified by index alone, so 17 is not "the optional field was
        // left out" and 19 is not "a newer field": both are documents whose element 16
        // might be a cell or might be a name.
        let elements = fixture_elements();

        let seventeen = array_of(&elements[..V2_ELEMENT_COUNT - 1]);
        let error = decode_payload(&seventeen).expect_err("17 elements is not 18");
        assert!(matches!(error, EncodeError::Decoding(_)), "got {error:?}");
        assert!(
            error.to_string().contains("geo_cell_macro"),
            "the refusal should name the element that went missing: {error}"
        );

        let mut nineteen = elements.clone();
        nineteen.push(Value::Integer(0.into()));
        assert!(matches!(
            decode_payload(&array_of(&nineteen)),
            Err(EncodeError::Decoding(_))
        ));
    }

    #[test]
    fn a_byte_field_written_as_an_array_of_integers_is_refused() {
        // The half of B9 that the library will not do for you: ciborium's
        // `deserialize_bytes` falls back to accepting a sequence as bytes, so without
        // `ByteBuf`'s visitor — which implements `visit_bytes` and nothing else — the
        // integer-array spelling would decode cleanly and mean something else.
        let mut elements = fixture_elements();
        elements[2] = Value::Array((0..32).map(|_| Value::Integer(0.into())).collect());

        let error = decode_payload(&array_of(&elements)).expect_err("an int array is not bytes");
        assert!(matches!(error, EncodeError::Decoding(_)), "got {error:?}");
        assert!(
            error.to_string().contains("byte string"),
            "the refusal should say what the element had to be: {error}"
        );
    }

    #[test]
    fn bytes_after_the_document_are_refused() {
        // One signature, one document. A buffer that keeps a second item behind the
        // payload is not a payload with a trailer — those bytes were never signed, and a
        // reader that ignored them would be attesting to a record that is partly
        // unsigned.
        let mut encoded = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();
        encoded.push(0x00);

        assert_eq!(
            decode_payload(&encoded),
            Err(EncodeError::TrailingBytes {
                document: encoded.len() as u64 - 1,
                offered: encoded.len(),
            })
        );
        // The same rule on the version probe, which is the other way in to a document.
        assert!(matches!(
            payload_version(&encoded),
            Err(EncodeError::TrailingBytes { .. })
        ));
    }

    #[test]
    fn a_map_claiming_version_two_is_refused() {
        // The confusion this version is written to make impossible: a keyed document
        // carrying v2's field names. Its keys would be in declaration order rather than
        // sorted, its byte fields would arrive as integer arrays, and a reader that
        // picked a shape by looking at the fields would sign up to all of that.
        assert_eq!(
            decode_payload(&document_declaring(u64::from(VERSION_V2))),
            Err(EncodeError::MismatchedShape {
                version: VERSION_V2,
                expected: "a positional CBOR array",
            })
        );
    }

    #[test]
    fn an_array_claiming_version_one_is_refused() {
        // The mirror image, and the reason the version number is what picks the shape:
        // v1's bytes are a map, and reading a positional document as v1 would silently
        // drop the six columns only v2 covers while still reporting a valid signature.
        let mut elements = fixture_elements();
        elements[0] = Value::Integer(VERSION_V1.into());

        assert_eq!(
            decode_payload(&array_of(&elements)),
            Err(EncodeError::MismatchedShape {
                version: VERSION_V1,
                expected: "a CBOR map with text keys",
            })
        );
        // The version probe alone still reads it: the document does declare 1, and it is
        // the shape that is wrong.
        assert_eq!(payload_version(&array_of(&elements)), Ok(VERSION_V1));
    }

    #[test]
    fn an_enum_code_no_table_assigns_is_refused() {
        // The document is well-formed and could be perfectly signed; what it cannot be is
        // stored, because there is no column value for code 9. Accepting it would mean a
        // row whose label accessor answers `None` for a NOT NULL column.
        for (element, field) in [(8usize, "adv_type"), (15usize, "location_source")] {
            let mut elements = fixture_elements();
            elements[element] = Value::Integer(9.into());

            let error =
                decode_payload(&array_of(&elements)).expect_err("code 9 is in neither table");
            assert_eq!(
                error,
                EncodeError::UnknownCode {
                    element,
                    field,
                    code: 9
                },
                "the refusal should name the element and the code"
            );
        }
    }

    #[test]
    fn the_labels_a_payload_claims_are_the_column_labels_their_codes_stand_for() {
        // What a verifier comparing a decoded payload against the row actually needs: the
        // wire carries a code, the column holds a label, and the tables are what connect
        // them.
        let encoded = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();
        let decoded = decode_payload(&encoded).unwrap();

        assert_eq!(decoded.adv_type_label(), Some("connectable_adv"));
        assert_eq!(decoded.location_source_label(), Some("node_gps"));

        // v1 carries neither: it has no code table of its own, and its map form never
        // covered the location source at all.
        let v1 = encode_payload(&CanonicalPayload::V1(v1_fixture())).unwrap();
        let decoded = decode_payload(&v1).unwrap();
        assert_eq!(decoded.adv_type_label(), None);
        assert_eq!(decoded.location_source_label(), None);
    }

    #[test]
    fn a_pinned_v2_document_verifies_over_its_stored_bytes() {
        // The auditor's path, on the new shape: take the bytes as stored, take the key,
        // check. Nothing here re-encodes, which is why the layout change cannot disturb a
        // signature that was made over these bytes.
        use crate::provenance::verify::verify_signature;

        let key = SigningKey::generate(&mut thread_rng());
        let bytes = hex::decode(V2_FIXTURE_HEX).expect("fixture is hex");
        let signature = key.sign(&bytes);

        let decoded = decode_payload(&bytes).expect("the fixture decodes as v2");
        verify_signature(&key.verifying_key(), &bytes, &signature).unwrap();
        assert_eq!(
            encode_payload(&decoded).unwrap(),
            bytes,
            "re-encoding the decoded fixture reproduces it byte for byte"
        );
    }

    #[test]
    fn every_field_of_version_two_is_inside_the_bytes() {
        // The defect v2 closes is that a column could be edited without disturbing the
        // signature. For each field, a payload that differs only in that field has to
        // differ on the wire too — otherwise a signature over the original would still
        // verify against the altered value.
        let base = full_v2();
        let base_bytes = encode_payload(&CanonicalPayload::V2(base.clone())).unwrap();

        type Mutation = (&'static str, fn(&mut PayloadV2));
        let mutations: Vec<Mutation> = vec![
            ("signal_type", |p| p.signal_type = 1),
            ("origin_node_id", |p| p.origin_node_id = vec![9u8; 32]),
            ("device_hash", |p| p.device_hash = vec![9u8; 32]),
            ("device_address", |p| {
                p.device_address = Some(vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
            }),
            ("observed_at", |p| {
                p.observed_at = "2026-08-15T12:00:00.123457+00:00".to_string()
            }),
            ("observed_at_node_local", |p| {
                p.observed_at_node_local = "2026-08-15T12:00:00.123455+00:00".to_string()
            }),
            ("rssi", |p| p.rssi = -68),
            ("tx_power", |p| p.tx_power = Some(5)),
            // The two enum elements are codes, so the mutation is a different code
            // rather than a different label: 1 is `scannable_adv`, 2 is
            // `interpolated`, and both are in their table.
            ("adv_type", |p| p.adv_type = Some(1)),
            ("location", |p| p.location = Some([40.6893, -74.0445])),
            ("alt_m", |p| p.alt_m = Some(13.5)),
            ("accuracy_m", |p| p.accuracy_m = Some(4.5)),
            ("location_source", |p| p.location_source = 2),
            ("geo_cell_fine", |p| {
                p.geo_cell_fine = Some(0x892a1072b5effff)
            }),
            ("geo_cell_macro", |p| {
                p.geo_cell_macro = Some(0x0892a1000001fff)
            }),
            ("signal_payload", |p| {
                p.signal_payload = b"{\"ble\":{}".to_vec()
            }),
            ("advertised_name", |p| {
                p.advertised_name = Some("OtherDevice".to_string())
            }),
        ];

        for (field, mutate) in mutations {
            let mut changed = base.clone();
            mutate(&mut changed);
            // Guards the test itself: a mutation that changes nothing would make the
            // assertion below pass for the wrong reason.
            assert_ne!(changed, base, "{field} mutation is a no-op");

            let bytes = encode_payload(&CanonicalPayload::V2(changed)).unwrap();
            assert_ne!(bytes, base_bytes, "{field} is not inside the signed bytes");
        }
    }

    // ==================== version dispatch ====================

    #[test]
    fn a_version_this_build_has_no_shape_for_is_refused() {
        // 3 is the future. Reading it as v1 because the field names happen to line up
        // would hand a reader an attestation that covers less than the document claims.
        let from_the_future = document_declaring(3);
        assert_eq!(payload_version(&from_the_future), Ok(3));
        assert_eq!(
            decode_payload(&from_the_future),
            Err(EncodeError::UnsupportedVersion(3))
        );
    }

    #[test]
    fn a_version_number_that_cannot_be_a_version_is_not_guessed_at() {
        // Zero is a legal u16 and no version, so it is refused as unsupported rather
        // than decoded as whichever shape is nearest.
        assert!(matches!(
            decode_payload(&document_declaring(0)),
            Err(EncodeError::UnsupportedVersion(0))
        ));
        // Something no u16 can hold is not a version number at all, and saying so is a
        // different answer from "unsupported".
        assert!(matches!(
            payload_version(&document_declaring(u64::MAX)),
            Err(EncodeError::InvalidData(_))
        ));
    }

    #[test]
    fn a_document_without_a_version_or_with_the_wrong_type_is_not_a_payload() {
        // Not every rejection is a version problem: a CBOR map that is not a payload,
        // and a payload whose version field is text, are both garbage input.
        let mut not_a_map = Vec::new();
        into_writer(&Value::Integer(7.into()), &mut not_a_map).unwrap();
        assert!(matches!(
            payload_version(&not_a_map),
            Err(EncodeError::InvalidData(_))
        ));

        let text_version = Value::Map(vec![(
            Value::Text("schema_version".to_string()),
            Value::Text("two".to_string()),
        )]);
        let mut bytes = Vec::new();
        into_writer(&text_version, &mut bytes).unwrap();
        assert!(matches!(
            payload_version(&bytes),
            Err(EncodeError::InvalidData(_))
        ));

        let empty_map = Value::Map(Vec::new());
        let mut bytes = Vec::new();
        into_writer(&empty_map, &mut bytes).unwrap();
        assert!(matches!(
            payload_version(&bytes),
            Err(EncodeError::InvalidData(_))
        ));
    }

    #[test]
    fn a_version_one_document_carrying_version_two_fields_is_rejected() {
        // Strictness with a reason: a document that declares 1 but carries `alt_m` is
        // either corrupt or a forgery aimed at whichever shape the reader happens to
        // pick. The reader picks none of them.
        let hybrid = {
            let fields = vec![
                (
                    Value::Text("schema_version".to_string()),
                    Value::Integer(1u8.into()),
                ),
                (Value::Text("alt_m".to_string()), Value::Float(1.0)),
            ];
            let mut buffer = Vec::new();
            into_writer(&Value::Map(fields), &mut buffer).unwrap();
            buffer
        };

        let error = decode_payload(&hybrid).expect_err("a v1 document cannot carry alt_m");
        assert!(matches!(error, EncodeError::Decoding(_)), "got {error:?}");
        assert!(
            error.to_string().contains("alt_m"),
            "the rejection should name the field it could not place: {error}"
        );
    }

    #[test]
    fn the_dispatch_follows_the_declared_version_not_the_field_set() {
        // v2 bytes never arrive as V1 and vice versa, whichever way the fields line up.
        let v2_bytes = encode_payload(&CanonicalPayload::V2(full_v2())).unwrap();
        assert!(matches!(
            decode_payload(&v2_bytes),
            Ok(CanonicalPayload::V2(_))
        ));

        let v1_bytes = encode_payload(&CanonicalPayload::V1(v1_fixture())).unwrap();
        assert!(matches!(
            decode_payload(&v1_bytes),
            Ok(CanonicalPayload::V1(_))
        ));
    }

    #[test]
    fn invalid_cbor_is_still_invalid() {
        let invalid_data = vec![0xFF, 0xFF, 0xFF, 0xFF];
        assert!(matches!(
            decode_payload(&invalid_data),
            Err(EncodeError::Decoding(_))
        ));
    }

    // ==================== signal_payload canonical bytes ====================

    #[test]
    fn canonical_signal_payload_is_compact_with_keys_in_map_order() {
        let value = serde_json::json!({"ble": {"adv_type": "connectable_adv"}, "rssi": -67});

        let bytes = canonical_signal_payload(&value).unwrap();

        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"ble":{"adv_type":"connectable_adv"},"rssi":-67}"#
        );
    }

    #[test]
    fn key_order_and_whitespace_are_not_part_of_what_gets_signed() {
        // This is what makes the rule reproducible from a JSONB column: Postgres keeps
        // neither the order the keys went in nor the spacing, and a reader re-serialising
        // what it gets back lands on the same bytes.
        let signed = canonical_signal_payload(&serde_json::json!({
            "ble": {"adv_type": "connectable_adv"},
            "rssi": -67,
        }))
        .unwrap();

        let from_a_database = serde_json::from_str::<serde_json::Value>(
            r#"{ "rssi" : -67 , "ble" : { "adv_type" : "connectable_adv" } }"#,
        )
        .unwrap();

        assert_eq!(signed, canonical_signal_payload(&from_a_database).unwrap());
    }

    #[test]
    fn values_that_differ_in_a_value_differ_in_the_signed_bytes() {
        let base = canonical_signal_payload(&serde_json::json!({"ble": {"name": "a"}})).unwrap();

        let renamed = canonical_signal_payload(&serde_json::json!({"ble": {"name": "b"}})).unwrap();
        let added = canonical_signal_payload(&serde_json::json!({
            "ble": {"name": "a"}, "position": {"origin": "gps"}
        }))
        .unwrap();
        let emptied = canonical_signal_payload(&serde_json::json!({})).unwrap();
        let null = canonical_signal_payload(&serde_json::Value::Null).unwrap();

        assert_ne!(base, renamed, "a value changed inside the payload");
        assert_ne!(base, added, "a key added inside the payload");
        assert_ne!(base, emptied);
        assert_eq!(null, b"null".to_vec(), "null is spelled, not omitted");
        assert_eq!(emptied, b"{}".to_vec());
    }

    // ==================== existing behaviour ====================

    #[test]
    fn version_one_round_trips() {
        let payload = v1_fixture();
        let encoded = encode_payload(&CanonicalPayload::V1(payload.clone())).unwrap();

        let CanonicalPayload::V1(decoded) = decode_payload(&encoded).unwrap() else {
            panic!("expected v1")
        };
        assert_eq!(payload, decoded);
    }

    #[test]
    fn version_one_with_every_optional_field_null_round_trips() {
        let payload = PayloadV1::builder()
            .schema_version(1)
            .signal_type(0)
            .origin_node_id(&[0u8; 32])
            .device_hash(&[1u8; 32])
            .observed_at_node_local("2026-08-15T12:00:00Z")
            .rssi(-67)
            .no_device_address()
            .no_tx_power()
            .no_adv_type()
            .no_location()
            .no_signal_payload()
            .no_advertised_name()
            .build();

        let encoded = encode_payload(&CanonicalPayload::V1(payload.clone())).unwrap();
        let CanonicalPayload::V1(decoded) = decode_payload(&encoded).unwrap() else {
            panic!("expected v1")
        };
        assert_eq!(payload, decoded);
    }

    #[test]
    fn a_large_signal_payload_survives_the_round_trip() {
        let large = vec![0u8; 1024];
        let payload = PayloadV2::builder()
            .origin_node_id(&[0u8; 32])
            .device_hash(&[1u8; 32])
            .observed_at(chrono::Utc::now())
            .observed_at_node_local(chrono::Utc::now())
            .rssi(-67)
            .location_source(LocationSource::NodeGps)
            .signal_payload(&large)
            .build();

        let encoded = encode_payload(&CanonicalPayload::V2(payload.clone())).unwrap();
        let CanonicalPayload::V2(decoded) = decode_payload(&encoded).unwrap() else {
            panic!("expected v2")
        };
        assert_eq!(payload, decoded);
        assert_eq!(decoded.signal_payload.len(), 1024);
    }

    #[test]
    fn payloads_that_differ_in_one_field_encode_differently() {
        let build = |rssi: i16| {
            PayloadV1::builder()
                .signal_type(0)
                .origin_node_id(&[0u8; 32])
                .device_hash(&[1u8; 32])
                .observed_at_node_local("2026-08-15T12:00:00Z")
                .rssi(rssi)
                .build()
        };

        let first = encode_payload(&CanonicalPayload::V1(build(-67))).unwrap();
        let second = encode_payload(&CanonicalPayload::V1(build(-72))).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn the_current_version_is_the_one_new_documents_carry() {
        assert_eq!(CURRENT_VERSION, VERSION_V2);
        assert_eq!(
            payload_version(&encode_payload(&CanonicalPayload::V2(full_v2())).unwrap()),
            Ok(CURRENT_VERSION)
        );
    }
}
