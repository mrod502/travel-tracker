# Canonical CBOR Encoding Specification

**Status:** ✅ **Implemented** — version 2 is what nodes write today; version 1 is frozen and
still decoded
**Date:** 2026-08-14
**Amended:** 2026-09-18 — the field set extended to eighteen elements for v2, the
`adv_type` and `location_source` code tables defined, and the example and encoding
algorithm replaced with the ones the implementation uses. See
[Version History](#version-history) at the end.
**Purpose:** Define byte-exact canonical encoding for `signed_payload` in occurrence provenance verification

---

## Overview

This document specifies the canonical CBOR encoding used to generate the `signed_payload` field stored with every occurrence record. The encoding must be **deterministic** — identical input data MUST produce identical byte output, regardless of platform, implementation, or execution order.

### Requirements

1. **Deterministic**: Same input → identical bytes every time
2. **Pure Rust**: No external C/C++ dependencies
3. **Schema evolution**: Support forward/backward compatibility via schema_version
4. **Compact**: Minimize wire size for constrained networks (LoRa/signal nodes)
5. **Verifiable**: Any party can recompute and verify signatures offline

### Why CBOR?

CBOR (RFC 8949) is chosen over alternatives because:

| Format | Deterministic? | Pure Rust? | Schema Evolution | Wire Size |
|--------|---------------|------------|------------------|-----------|
| CBOR (ciborium) | ✅ Yes (RFC 8949) | ✅ Yes | ✅ Via schema_version | ✅ Compact |
| MessagePack | ⚠️ Partial | ✅ Yes | ❌ Limited | ✅ Compact |
| JSON | ❌ No canonical form | ✅ Yes | ✅ Via schema_version | ❌ Verbose |
| Protobuf | ✅ Yes | ❌ C++ deps | ✅ Strong | ✅ Compact |
| bincode | ❌ Not for signing | ✅ Yes | ❌ No | ✅ Compact |

**Selected crate:** `ciborium` (pure Rust, serde-compatible, CBOR RFC 8949 compliant)

---

## Field Set (Signed Content)

The signed payload is a **CBOR array** whose fields appear in **exact order**. Version 2
is the shape nodes write today; it has eighteen elements — the twelve this specification
has always indexed, plus six appended at the end under the "new fields go last" rule in
[Schema Versioning Strategy](#schema-versioning-strategy).

```
signed_payload = canonical_encode(
    0. schema_version     -- UInt (SMALLINT)
    1. signal_type        -- UInt (byte: 0=bluetooth, 1=wifi, etc.)
    2. origin_node_id     -- Bytes (32-byte SHA-256 hash)
    3. device_hash        -- Bytes (32-byte SHA-256 hash)
    4. device_address     -- Optional Bytes (6 bytes for BLE/WiFi)
    5. observed_at_node_local -- Text (ISO 8601 timestamp, UTC)
    6. rssi               -- Int (SMALLINT, signed)
    7. tx_power           -- Optional Int (SMALLINT, signed)
    8. adv_type           -- Optional UInt (see the code table below)
    9. location           -- Optional array [lat, lon] if from node GPS
    10. signal_payload    -- Bytes (the canonical form of the signal-specific data)
    11. advertised_name   -- Optional text (may be empty or null)

    -- appended for version 2:
    12. observed_at       -- Text (ISO 8601, microsecond precision, UTC): the
                            sync-corrected observation time, i.e. the instant the row's
                            partition key and query results will report
    13. alt_m             -- Optional Float (`REAL`; altitude above mean sea level)
    14. accuracy_m        -- Optional Float (`REAL`; horizontal accuracy estimate)
    15. location_source   -- UInt (see the code table below; the column is NOT NULL)
    16. geo_cell_fine     -- Optional UInt (resolution 9 H3 cell, a generated column)
    17. geo_cell_macro    -- Optional UInt (resolution 6 H3 cell, a generated column)
)
```

**Critical ordering:** Fields MUST be encoded in this exact order (indexed 0-17 above) to
ensure determinism. This is enforced by using a Rust struct with explicit field ordering,
not a HashMap.

**An element is never omitted.** An element's meaning is its index, so an absent optional
field is CBOR `null` (`0xF6`) in that position — never a document with one fewer element.
A decoder MUST refuse an array whose element count differs from its declared version's,
because 17 elements is not "the optional field was left out": it is a document in which
element 16 might be a cell or might be a name.

**Version 1's shape is historical.** The v1 documents already stored were written as a map
with text keys — deterministic, keys in Rust field-declaration order, and with `Vec<u8>`
fields spelled as arrays of integers rather than byte strings. That is not §4.2.1
canonical. It is nevertheless frozen: those bytes were signed by nodes that no longer run
this code, so v1 is decoded and never re-encoded, and the version number — not the
document's shape — decides which rules apply to it.

**Key changes from previous spec:**
- `device_hash` is now **Bytes (32-byte SHA-256 hash)** instead of Text (hex string)
- `device_address` is now **Optional Bytes** instead of Text
- `signal_type` added as explicit field
- Signal-specific fields (service_uuids, manufacturer_data, raw_payload_hex) moved into `signal_payload` for extensibility

### Code tables

Two elements carry a known enum as an integer. The numbers belong to **this
specification**, not to the database: the `adv_type` and `location_source` columns are
PostgreSQL enums, whose labels are ordered by creation rather than by meaning, so nothing
server-side assigns `connectable_adv` the number 0. A decoder MUST refuse a code outside
these tables rather than store a value no column can hold.

**Element 8 — `adv_type`** (absent, i.e. `null`, when the capture layer reported none):

| Code | Label | Meaning |
|------|-------|---------|
| 0 | `connectable_adv` | ADV_IND: connectable and scannable |
| 1 | `scannable_adv` | ADV_SCAN_IND |
| 2 | `broadcast_adv` | ADV_NONCONN_IND |
| 3 | `extended_adv` | An extended advertisement |

**Element 15 — `location_source`** (always present; the column is `NOT NULL`):

| Code | Label | Meaning |
|------|-------|---------|
| 0 | `node_fixed` | The origin node's configured fixed position |
| 1 | `node_gps` | The origin node's own GPS fix |
| 2 | `interpolated` | Derived between two fixes, not measured |
| 3 | `aggregator_fixed` | A position supplied by the aggregating node |

Labels are the column's own spelling (`repo/tests/wire_types.rs` pins them against
`pg_enum`), which is what lets a reader that has decoded a payload compare it with the
row. Codes are assigned once and never reused; adding a variant means adding a row here
and bumping nothing else, while *reordering* an existing assignment would silently
reattach every stored signature to a different value.

---

## CBOR Deterministic Encoding Rules

Per RFC 8949 Section 4.2.1 (Core Deterministic Encoding Requirements):

### 1. **Preferred Serialization**
- Integers must use the shortest encoding possible
- Floats must use shortest encoding that preserves value (binary16/32/64)
- Definite-length encoding for all strings, arrays, and maps

### 2. **No Indefinite Lengths**
- All arrays must specify length upfront
- All maps must specify length upfront
- No streaming/indefinite-length encoding

### 3. **No Maps in the Signed Payload**
- The 2026-08-14 recommendation — "avoid maps in signed payload; use ordered structs" — is
  now the rule: a version 2 payload contains no map anywhere, so the key-ordering question
  never arises and the document is core-deterministic by construction
- Were a map ever introduced, its keys would have to be sorted in bytewise lexicographic
  order of their encodings
- A decoder refuses a version 2 document that carries a map head, and a version 1 document
  that is not a map: the declared version picks the shape, and a document that disagrees
  with itself is refused rather than read against whichever shape happens to fit

### 4. **Numeric Constraints**
- Integers encoded in minimal bytes
- No mixing of integer and floating-point representations

### 5. **One Document Per Signature**
- The `signed_payload` column holds exactly one CBOR item and nothing else
- A decoder MUST refuse a buffer that has bytes left over after the document, because
  CBOR does not require a reader to notice them: those trailing bytes belong to nothing
  that was signed, and a verifier that ignored them would be attesting to a record that is
  partly unsigned

---

## Type Mappings

### Rust → CBOR Mapping Table

| Rust Type | CBOR Type | Encoding Notes |
|-----------|-----------|----------------|
| `u16` (schema_version) | `uint` | Minimal encoding (major type 0) |
| `u8` (signal_type) | `uint` | Byte value for signal type |
| `u8` (`adv_type`, `location_source`) | `uint` | A code from [the tables above](#code-tables); `null` for an absent `adv_type` |
| `i16` (rssi, tx_power) | `nint` or `uint` | Per CBOR signed integer rules (major type 1 for negatives); `-67` is `38 42` |
| `Vec<u8>` (origin_node_id, device_hash, signal_payload) | `bytes` | 32-byte hashes, definite-length (major type 2). **The encoder must ask for `bytes` explicitly:** serde drives a bare `Vec<u8>` through `serialize_seq`, which emits an array of integers (major type 4) and is the spelling this format refuses |
| `Option<Vec<u8>>` (device_address) | `bytes` or `null` | 6 bytes for a BLE MAC |
| `String` (timestamps, advertised_name) | `text_string` | UTF-8, definite-length (major type 3). Timestamps are microsecond-precision UTC — `TIMESTAMPTZ` stores microseconds, so a nanosecond spelling could not be reproduced from the column it came from |
| `Option<[f64; 2]>` (location) | `array` or `null` | Definite-length array of two floats if present (major type 4) |
| `Option<f32>` (alt_m, accuracy_m) | `float` or `null` | The columns are `REAL`, so f32 is what the row can show; the shortest width that round-trips (`12.5` → `F9 4A 40`) |
| `Option<u64>` (geo_cell_fine, geo_cell_macro) | `uint` or `null` | An H3 cell is a full 64-bit integer, so the head is `1B` plus eight bytes |

### Example: Location Field

```rust
// If location is determined by origin node GPS:
location = Some([40.6892_f64, -74.0445_f64])
// CBOR: 82                              # array(2)
//         FB 40 44 58 37 B4 A2 33 9C    #  40.6892, as binary64
//         FB C0 52 82 D9 16 87 2B 02    # -74.0445, as binary64

// If location NOT determined (signal node via aggregator):
location = None
// CBOR: 0xF6 (CBOR null)
```

---

## Canonical Encoding Algorithm

### Step 1: Build the payload (Rust)

`PayloadV2` (`app/src/provenance/payload.rs`) declares its fields in exactly the order of
the layout above — field order *is* element order — and `V2_ELEMENT_COUNT` names how many
there are. It carries **no serde derive**: a derive would emit a map, and would send a
`Vec<u8>` out as an array of integers, so both `Serialize` and `Deserialize` are hand-written
against this layout.

```rust
pub struct PayloadV2 {   // declaration order == element index
    pub schema_version: u16,              //  0
    pub signal_type: u8,                  //  1
    pub origin_node_id: Vec<u8>,          //  2  bytes
    pub device_hash: Vec<u8>,             //  3  bytes
    pub device_address: Option<Vec<u8>>,  //  4  bytes | null
    pub observed_at_node_local: String,   //  5
    pub rssi: i16,                        //  6
    pub tx_power: Option<i16>,            //  7
    pub adv_type: Option<u8>,             //  8  code | null
    pub location: Option<[f64; 2]>,       //  9
    pub signal_payload: Vec<u8>,          // 10  bytes
    pub advertised_name: Option<String>,  // 11
    pub observed_at: String,              // 12
    pub alt_m: Option<f32>,               // 13
    pub accuracy_m: Option<f32>,          // 14
    pub location_source: u8,              // 15  code
    pub geo_cell_fine: Option<u64>,       // 16
    pub geo_cell_macro: Option<u64>,      // 17
}
```

Producers build it through `PayloadV2::builder()`, which takes the `AdvType` and
`LocationSource` **enums** and applies the code tables itself:

```rust
let payload = PayloadV2::builder()
    .signal_type(0)
    .origin_node_id(&vec![0u8; 32])
    .device_hash(&vec![1u8; 32])
    .device_address(&vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
    .observed_at(observed)                      // DateTime<Utc>, microseconds
    .observed_at_node_local(local)
    .rssi(-67)
    .tx_power(4)
    .adv_type(AdvType::ConnectableAdv)          // -> code 0
    .location(40.6892, -74.0445)
    .alt_m(12.5)
    .accuracy_m(4.0)
    .location_source(LocationSource::NodeGps)   // -> code 1
    .geo_cell_fine(0x892a1072b5bffff)
    .geo_cell_macro(0x0892a1000000fff)
    .signal_payload(br#"{"ble":{"adv_type":"connectable_adv"}}"#)
    .advertised_name("TestDevice")
    .build();
```

Because the setters take enums, a producer cannot put a code in that no table assigns; the
only way to get a raw code in is to decode one.

### Step 2: Serialize to CBOR (deterministic)

```rust
use ciborium::ser::into_writer;

let mut buffer = Vec::new();
into_writer(&payload, &mut buffer).unwrap();
// buffer now contains canonical CBOR bytes
```

`impl Serialize for PayloadV2` opens with `serializer.serialize_tuple(V2_ELEMENT_COUNT)`,
which writes a definite-length array head (`0x92` for eighteen elements) and then the
elements in declaration order. The four byte-carrying fields are wrapped in a private
newtype whose `Serialize` calls `serialize_bytes`, because serde would otherwise hand
ciborium a *sequence of integers* for a `Vec<u8>`.

**Configuration required:**
- `ciborium` with default settings — no custom serializer, no custom writer
- Byte fields serialized through the wrapper that asks for major type 2 explicitly
- Preferred serialization (shortest head, shortest float width) left to ciborium — and
  then **verified, not assumed**: `floats_use_the_shortest_form_that_keeps_the_value` and
  `the_encoder_puts_no_maps_anywhere_in_a_payload` walk the encoded heads with a reader
  written independently of the encoder

### Step 3: Sign the Buffer

```rust
use ed25519_dalek::Signer;

let signature = private_key.sign(&buffer);
```

### Step 4: Store in Database

```sql
INSERT INTO occurrences (
    signed_payload,  -- BYTEA = canonical CBOR buffer
    signature        -- BYTEA = 64-byte Ed25519 signature
) VALUES (...);
```

---

## Complete Example

The bytes below are the checked-in golden vector — `V2_FIXTURE_HEX` in
`app/src/provenance/encode.rs`. The encoder is asserted against it byte-for-byte by
`version_two_bytes_are_pinned`, so this listing is the format, not a sketch of it.

### Input Data

```rust
let observed = chrono::DateTime::parse_from_rfc3339("2026-08-15T12:00:00.123456Z")
    .unwrap().with_timezone(&chrono::Utc);
let local = chrono::DateTime::parse_from_rfc3339("2026-08-15T12:00:00.123999Z")
    .unwrap().with_timezone(&chrono::Utc);

let payload = PayloadV2::builder()
    .signal_type(0)
    .origin_node_id(&vec![0u8; 32])
    .device_hash(&vec![1u8; 32])
    .device_address(&vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
    .observed_at(observed)                     // the corrected time -> element 12
    .observed_at_node_local(local)             // the clock-as-read  -> element 5
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
    .build();
```

### CBOR Output (245 bytes, definite-length, no map anywhere)

```
92                              # array(18) — the head IS the arity declaration
  02                            #  0 schema_version         uint 2
  00                            #  1 signal_type            uint 0 (bluetooth)
  58 20 00×32                   #  2 origin_node_id         bytes(32)
  58 20 01×32                   #  3 device_hash            bytes(32)
  46 aabbccddeeff               #  4 device_address         bytes(6)
  78 20 "2026-08-15T12:00:00.123999+00:00"
                                #  5 observed_at_node_local text(32)
  38 42                         #  6 rssi                   nint −67
  04                            #  7 tx_power               uint 4
  00                            #  8 adv_type               uint 0 → connectable_adv
  82                            #  9 location               array(2)
     fb 40445837b4a2339cfb      #      40.6892   (binary64)
     fb c05282d916872b02        #     −74.0445   (binary64)
  58 26 7b22626c65223a7b226164765f74797065223a22636f6e6e65637461626c655f616476227d7d
                                # 10 signal_payload         bytes(38)
  6a "TestDevice"               # 11 advertised_name        text(10)
  78 20 "2026-08-15T12:00:00.123456+00:00"
                                # 12 observed_at            text(32)
  f9 4a40                       # 13 alt_m                  half 12.5
  f9 4400                       # 14 accuracy_m             half 4.0
  01                            # 15 location_source        uint 1 → node_gps
  1b 0892a1072b5bffff           # 16 geo_cell_fine          uint64
  1b 00892a1000000fff           # 17 geo_cell_macro         uint64
```

Note what is *not* there: no key strings, no tag wraps, no indefinite lengths, and no
element that got dropped because it was empty. `12.5` and `4.0` are binary16 because that is
the shortest width that round-trips; the H3 cells take the 9-byte `1b` head because a cell
does not fit in 32 bits.

The same builder with every optional element unset encodes as **the same eighteen elements**
with `f6` (null) at 4, 7, 8, 9, 11, 13, 14, 16 and 17 — see
`an_empty_version_two_document_is_still_eighteen_elements`.

### Signature Verification (Verification Party)

```rust
// 1. Load occurrence row from database
let (signed_payload, signature) = load_occurrence(occurrence_id);

// 2. Verify over the stored bytes. These bytes are the signed content; nothing is
//    re-encoded from the row's columns.
let public_key = load_public_key(origin_node_id);
let ed25519_sig = ed25519_dalek::Signature::from_bytes(&signature);
public_key.verify(&signed_payload, &ed25519_sig)?;   // Err => tampering, reject

// 3. Decode for inspection / column cross-checks (strict; see Schema Versioning below)
let payload = decode_payload(&signed_payload)?;
```

---

## Schema Versioning Strategy

The `schema_version` value — element 0 of an array, or the `schema_version` key of a map —
selects the rules a decoder applies. It is read before the payload is decoded, and a value
this build does not know is refused rather than guessed at.

### Version 1 (historical — decoded, never written)

A CBOR **map with text keys**, holding the twelve keys `schema_version`, `signal_type`,
`origin_node_id`, `device_hash`, `device_address`, `observed_at_node_local`, `rssi`,
`tx_power`, `adv_type`, `location`, `signal_payload`, `advertised_name`. Keys appear in Rust
field-declaration order and the byte-carrying fields go out as arrays of integers, so a v1
document is deterministic but not §4.2.1 canonical.

That is accepted and closed: the v1 bytes already stored were signed by nodes that no longer
run this code, and re-encoding them would invalidate every one of those signatures. v1 is
therefore frozen — decoded for audit and history, never produced again. `tx_power`,
`adv_type`, `signal_payload` and `advertised_name` were declared in v1 and never set by a
producer, which is the main reason v2 exists.

### Version 2 (current — what nodes write)

The eighteen-element positional array specified above, with `signal_payload` and
`advertised_name` actually populated and the row-coverage fields (12–17) signed alongside
the rest.

### Adding fields later

A new field is **appended at the end** and the version number is bumped. Never insert,
reorder, or delete an element: element *n* means the same thing in every document of a given
version, and an insertion would silently reattach every stored signature to different values
while still decoding cleanly.

A version bump is the only compatibility mechanism, and it is deliberately blunt — a decoder
that does not know version *n* refuses the document instead of reading the prefix it happens
to recognise and reporting the rest as absent.

### Version Detection

`payload_version` reads the declared version without decoding the payload, understanding both
shapes:

```rust
match payload_version(&signed_payload)? {
    VERSION_V1 => { /* map with text keys */ }
    VERSION_V2 => { /* positional array of V2_ELEMENT_COUNT */ }
    other      => Err(EncodeError::UnsupportedVersion(other)),
}
```

`decode_payload` then requires the document's **shape to agree with its declared version**:
a version 1 document must be a map, a version 2 document must be an array
(`a_map_claiming_version_two_is_refused`, `an_array_claiming_version_one_is_refused`). A
document that contradicts itself is not read against whichever shape happens to fit — the
version a signature covers is the only thing telling the reader what the bytes mean.

On top of that, decoding a v2 array is strict about:
- **arity** — exactly `V2_ELEMENT_COUNT` elements, no more (`a_version_two_array_with_the_wrong_number_of_elements_is_refused`)
- **byte strings** — a binary field written as an array of integers is refused, even though
  ciborium's reader would happily accept one (`a_byte_field_written_as_an_array_of_integers_is_refused`)
- **codes** — an `adv_type` or `location_source` outside the tables (`an_enum_code_no_table_assigns_is_refused`)
- **one document** — bytes after the first complete item (`bytes_after_the_document_are_refused`)

---

## Implementation Checklist

### For Signer (Origin Node)

- [ ] Build the payload through `PayloadV2::builder()` — field order is element order, and
      the builder applies the `adv_type` / `location_source` code tables from the enums
- [ ] Encode with `ciborium::ser::into_writer` and no custom serializer or writer
- [ ] Spell every binary field as a CBOR byte string (the payload type does this; anything
      new added here must too)
- [ ] Timestamps as UTC RFC 8601 with microsecond precision
- [ ] An absent optional field is `null` **in its position** — never a shorter array
- [ ] Sign the exact bytes that go in the column, and store those bytes with the signature
- [ ] Never re-encode a stored payload to compare it with a row

### For Verifier (Any Node)

- [ ] Load the occurrence row (including `signed_payload` and `signature`)
- [ ] Look up `signing_public_key` from the `nodes` table
- [ ] Verify Ed25519 over the **stored bytes**, before trusting anything decoded from them
- [ ] Check the node is not revoked
- [ ] Reject if the signature fails or the node is revoked
- [ ] `payload_version` → `decode_payload`, and treat every refusal it returns as fatal:
      unknown version, shape that disagrees with its version, wrong arity, an integer array
      where a byte string belongs, an unknown enum code, trailing bytes
- [ ] When comparing payload values against the row, map codes back through the tables — a
      payload's `1` and the column's `node_gps` are the same claim

---

## Dependencies (Rust)

Add to `app/Cargo.toml`:

```toml
[dependencies]
ciborium = "0.2"              # Pure Rust CBOR serialization
ed25519-dalek = "2.0"         # Ed25519 signatures (pure Rust)
hex = "0.4"                   # Hex encoding/decoding
```

**All pure Rust, no C/C++ dependencies.**

---

## Security Considerations

### 1. **Canonicalization Attacks**

If the encoding is non-deterministic (e.g., HashMap ordering), an attacker could:
- Modify field ordering
- Change numeric representation (1 vs 1.0)
- Alter timestamp format (ISO 8601 vs Unix epoch)

**Mitigation:** A positional array with a fixed, declared element count — there is no key
ordering to exploit, and no map anywhere in the document. Preferred serialization is asserted
by a test that re-reads the heads rather than trusted to the library.

### 2. **Type Confusion**

If a verifier uses different types than the signer:
- String "123" vs integer 123
- Float vs integer representation
- A byte string vs an array of integers holding the same bytes
- Different timestamp formats

**Mitigation:** Strict type mappings as defined above, enforced on read: the decoder accepts
a byte string where a byte string belongs and refuses the integer-array spelling, and it
refuses an enum code no table assigns instead of storing a value the column cannot hold.

### 3. **Schema Drift**

If encoder/decoder versions differ:
- Missing fields in old decoder
- Extra fields in new encoder

**Mitigation:** `schema_version` plus exact arity. A document whose element count differs from
its declared version's is refused, so an extra or missing element cannot be misread as "a
field that happens to be absent".

### 4. **Timing Attacks**

Signature verification timing might leak information:
- Early exit on first mismatched byte

**Mitigation:** Use constant-time comparison libraries (ed25519-dalek does this)

---

## Testing

The requirements above are each pinned by a named test in `app/src/provenance/`.

### The format is pinned, not described

| Requirement | Test |
|---|---|
| Golden bytes — the encoder emits exactly the vector in [Complete Example](#complete-example) | `version_two_bytes_are_pinned` |
| Determinism — same input, identical bytes every time | `version_two_encoding_is_deterministic` |
| Array head carries the declared arity | `a_version_two_document_is_an_array_of_the_declared_number_of_elements` |
| Every element has the major type this spec names, in order | `every_element_is_the_type_the_layout_names` |
| Binary fields are byte strings, never integer arrays | `the_binary_fields_are_byte_strings_not_arrays_of_integers` |
| No map, no tag anywhere in the document | `the_encoder_puts_no_maps_anywhere_in_a_payload` |
| §4.2.1 preferred serialization for floats | `floats_use_the_shortest_form_that_keeps_the_value` |
| An absent optional is `null` in place, not a shorter array | `an_empty_version_two_document_is_still_eighteen_elements` |

The checks above re-read the encoded bytes with a head-walker written independently of
the encoder (`heads` / `read_head` in `encode.rs`'s test module), which also asserts integer
heads are minimal and rejects indefinite lengths. Preferred serialization is thus *measured*,
not inherited on trust from the library.

### Reading is strict

`a_version_two_array_with_the_wrong_number_of_elements_is_refused`,
`a_byte_field_written_as_an_array_of_integers_is_refused`,
`bytes_after_the_document_are_refused`,
`a_map_claiming_version_two_is_refused`, `an_array_claiming_version_one_is_refused`,
`an_enum_code_no_table_assigns_is_refused`, `a_version_this_build_has_no_shape_for_is_refused`,
`a_version_number_that_cannot_be_a_version_is_not_guessed_at`,
`a_document_without_a_version_or_with_the_wrong_type_is_not_a_payload`,
`invalid_cbor_is_still_invalid`.

### Round-trip, coverage, and the code tables

`a_version_two_document_round_trips_every_field`,
`a_version_two_document_with_every_optional_field_absent_round_trips`,
`every_field_of_version_two_is_inside_the_bytes` (mutating any single field changes the
bytes — so no field can be silently dropped by the encoder),
`payloads_that_differ_in_one_field_encode_differently`,
`every_enum_variant_has_its_own_code_and_the_code_maps_back_to_the_column_label`,
`a_code_outside_the_table_reads_as_no_label_at_all`,
`the_labels_a_payload_claims_are_the_column_labels_their_codes_stand_for`.

### Version 1 stays byte-identical

`the_v1_encoder_is_untouched_by_the_new_version`,
`a_version_one_document_from_before_the_change_still_decodes`,
`a_version_one_signature_still_verifies_over_its_stored_bytes`,
`a_version_one_document_carrying_version_two_fields_is_rejected`,
`the_dispatch_follows_the_declared_version_not_the_field_set`.

### Signature over stored bytes

`a_pinned_v2_document_verifies_over_its_stored_bytes` — a document is verified against the
bytes as stored; the row's columns are never re-encoded and compared.
`covers_row_separates_the_versions_that_attest_to_different_amounts` records which columns
each version can be trusted to attest.

Cross-platform determinism follows from the golden vector: any implementation that produces
these 245 bytes agrees with every other one, so it does not need to be re-measured per
platform to be checked.

---

## References

- **CBOR RFC 8949:** https://datatracker.ietf.org/doc/html/rfc8949
- **Deterministic Encoding:** https://datatracker.ietf.org/doc/html/rfc8949#section-4.2.1
- **ciborium crate:** https://crates.io/crates/ciborium
- **Ed25519 Dalek:** https://crates.io/crates/ed25519-dalek
- **Provenance Architecture:** [`provenance.md`](provenance.md)

---

## Version History

### 2026-09-18 — eighteen-element v2, code tables, real example

- **Field Set extended from twelve to eighteen elements.** Elements 0–11 keep their indices
  and meanings; `observed_at`, `alt_m`, `accuracy_m`, `location_source`, `geo_cell_fine` and
  `geo_cell_macro` are appended at 12–17 under the new-fields-last rule. A signature now
  covers the row it accompanies — the sync-corrected time that decides the partition key, and
  the position and cell a query will answer with — instead of a subset of it.
- **`adv_type` (element 8) and `location_source` (element 15) are integer codes.** This is the
  only place the numbers exist: the columns are PostgreSQL enums whose ordering is creation
  order, not meaning, so nothing server-side assigns `connectable_adv` the value 0. The labels
  the codes name are the columns' own spelling, pinned by `repo/tests/wire_types.rs`.
- **"Avoid maps" promoted from recommendation to rule** (§3), and **§5 "One Document Per
  Signature"** added. A v2 payload has no map anywhere, so §4.2.1's key-ordering question does
  not arise.
- **Type Mappings corrected:** `Vec<u8>` must be encoded with an explicit `serialize_bytes`
  call (serde's default for it is an integer array); the `REAL`/`BIGINT` columns added the
  `Option<f32>` and `Option<u64>` rows; the location example was rewritten — `[81 0x01, …]` is
  not CBOR.
- **Canonical Encoding Algorithm and Complete Example replaced** with the positional-array
  encoder actually in use and the checked-in 245-byte golden vector. The illustrative example
  that was there described a field set (`raw_payload_hex`, `address_type`, `service_uuids`,
  `manufacturer_data`) that the schema does not have.
- **Schema Versioning Strategy rewritten:** v1 is a historical text-keyed map that stays
  byte-identical because its bytes are already signed; v2 is the array. A decoder picks the
  shape from the declared version and refuses a document that disagrees with itself.
- **Testing section now names the tests** that hold each rule, including the golden-vector
  check that makes B9 ("no canonical vector exists") false.

*Why v2 was redefined in place rather than bumped to 3:* the twelve-element **map** form that
briefly existed under the name "v2" was never committed and no version 2 rows exist in any
database, so there was nothing to preserve. Version 1 was untouched.

### 2026-08-14 — original specification

Twelve-field signed set, `ciborium` selected over MessagePack/JSON/Protobuf/bincode, RFC 8949
§4.2.1 adopted as the determinism rule, `device_hash`/`device_address` moved from hex text to
bytes, signal-specific fields folded into `signal_payload`.

---

**Last Updated:** 2026-09-18
**Status:** ✅ **Implemented** — version 2 is what nodes write; version 1 is frozen and still
decoded. Layout and code tables: `app/src/provenance/payload.rs`. Encoding, decoding,
verification and the conformance tests: `app/src/provenance/encode.rs`.
