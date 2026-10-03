---
name: canonical-cbor
description: Canonical CBOR encoding for signed occurrence payloads
source: custom
---

# Canonical CBOR Encoding for Travel System

Authoritative spec: `.knowledge/architecture/canonical-cbor-spec.md`. Implementation:
`app/src/provenance/payload.rs` (layout + code tables) and `app/src/provenance/encode.rs`
(encode / decode / verify + conformance tests). **Read the spec before changing the layout** —
these bytes are signed, so a change here invalidates stored signatures.

## When to Use

- **Signing occurrence data** when creating signed provenance for signal detections (Bluetooth, WiFi, etc.)
- **Verifying occurrence signatures** when receiving relayed data from other nodes
- **Ensuring deterministic encoding** across platforms for signature verification
- **Schema versioning** when evolving the signed payload structure over time

## The shape, in one paragraph

Version 2 — what nodes write — is a **CBOR positional array of 18 elements**
(`V2_ELEMENT_COUNT`), no map anywhere in it, binary fields spelled as **byte strings** (major
type 2), enum fields spelled as **integer codes** from the spec's tables. Version 1 is a
**text-keyed map** that is frozen: those bytes were signed by nodes that no longer run this
code, so v1 is decoded and never re-encoded. `PayloadV2` has **no serde derive**, because the
derive would emit a map and would turn every `Vec<u8>` into an array of integers.

## Procedure

### 1. Build the payload — never hand-roll a struct

```rust
use repo::models::enums::{AdvType, LocationSource};
use crate::provenance::payload::PayloadV2;

let payload = PayloadV2::builder()
    .signal_type(0)                                  // 0 = bluetooth
    .origin_node_id(&node_id_32_bytes)
    .device_hash(&device_hash_32_bytes)
    .device_address(&six_byte_mac)                   // Option
    .observed_at(observed_utc)                       // DateTime<Utc> -> element 12
    .observed_at_node_local(local_utc)               // DateTime<Utc> -> element 5
    .rssi(-67)
    .tx_power(4)                                     // Option
    .adv_type(AdvType::ConnectableAdv)               // enum, not a raw code
    .location(40.6892, -74.0445)                     // Option
    .alt_m(12.5)                                     // Option<f32>  (REAL column)
    .accuracy_m(4.0)                                 // Option<f32>
    .location_source(LocationSource::NodeGps)        // enum, not a raw code
    .geo_cell_fine(0x892a1072b5bffff)                // Option<u64> (res-9 H3)
    .geo_cell_macro(0x0892a1000000fff)               // Option<u64> (res-6 H3)
    .signal_payload(br#"{"ble":{...}}"#)
    .advertised_name("TestDevice")                   // Option
    .build();
```

Element order is the struct's **declaration order**, and `PayloadV2::from_occurrence` is the
path a stored row takes. The setters take the `AdvType` / `LocationSource` **enums** and apply
the code tables, so a producer cannot emit a code that no table assigns.

If a genuinely new field is needed: **append it at the end**, bump the version, update
`V2_ELEMENT_COUNT`, the spec's Field Set, the golden vector and the `every_element_is_the_type_
the_layout_names` major-type list. Never insert, reorder, or delete an element.

### 2. Serialize

```rust
use ciborium::ser::into_writer;

let mut buffer = Vec::new();
into_writer(&payload, &mut buffer).expect("CBOR serialization failed");
```

Default `ciborium`, no custom serializer, no custom writer. `impl Serialize for PayloadV2`
opens with `serializer.serialize_tuple(V2_ELEMENT_COUNT)` (definite-length array head, `0x92`)
and wraps each byte-carrying field in the private `ByteStr` newtype so serde calls
`serialize_bytes`. An absent `Option` is CBOR `null` (`0xF6`) **in its position** — never a
shorter array.

### 3. Sign the exact bytes

```rust
use ed25519_dalek::Signer;

let signature = private_key.sign(&buffer);   // 64 bytes
```

Store `buffer` **verbatim** in `occurrences.signed_payload`. Those bytes are the signed
content; nothing re-encodes them later to compare against the row.

### 4. Verify (verifier side)

```rust
use crate::provenance::encode::{decode_payload, payload_version};
use ed25519_dalek::Verifier;

let (signed_payload, signature_bytes) = load_occurrence(occurrence_id);

// 1. Version first — understands both the v1 map and the v2 array
let version = payload_version(&signed_payload)?;

// 2. Verify over the stored bytes, before trusting anything decoded from them
let public_key = load_node_public_key(origin_node_id)?;
let sig = ed25519_dalek::Signature::from_bytes(&signature_bytes);
public_key.verify(&signed_payload, &sig)?;

// 3. Decode for inspection / column cross-checks
let payload = decode_payload(&signed_payload)?;
```

Check the node is not revoked. Treat every refusal from `payload_version` / `decode_payload`
as fatal — see Pitfalls.

### 5. Comparing a decoded payload with the row

The wire carries integers where the row carries enum labels, so compare through the tables:

```rust
use crate::provenance::payload::{adv_type_label, location_source_label};

assert_eq!(location_source_label(payload.location_source), Some(row.location_source.as_str()));
```

A code no table assigns reads as `None`, and `decode_payload` already refused it earlier.

## Type Mappings

| Rust Type | CBOR Type | Notes |
|-----------|-----------|-------|
| `u16` (schema_version) | `uint` | Minimal encoding (major type 0); element 0 |
| `u8` (`adv_type`, `location_source`) | `uint` | A code from the spec's tables; `adv_type` may be `null` |
| `i16` (rssi, tx_power) | `nint` or `uint` | `-67` is `38 42` |
| `Vec<u8>` | `bytes` | Major type 2. **Needs an explicit `serialize_bytes`** — serde's default for `Vec<u8>` is an integer array, which this format refuses |
| `Option<Vec<u8>>` | `bytes` or `null` | CBOR null = `0xF6` |
| `String` | `text_string` | Timestamps: UTC RFC 8601, **microseconds** (`TIMESTAMPTZ` precision) |
| `Option<[f64; 2]>` (location) | `array(2)` or `null` | `[lat, lon]`, both binary64 |
| `Option<f32>` (alt_m, accuracy_m) | `float` or `null` | `REAL` columns; shortest width that round-trips (`12.5` → `F9 4A 40`) |
| `Option<u64>` (geo cells) | `uint` or `null` | A cell needs the 9-byte `1B` head |

## CBOR Deterministic Rules (RFC 8949 §4.2.1)

1. **Preferred serialization**: shortest integer heads; floats narrowed to the shortest width
   that round-trips (ciborium does this; the test suite re-reads the heads to confirm)
2. **No indefinite lengths**: every array specifies its length upfront
3. **No maps in the signed payload**: with no map there is no key order to agree on
4. **Fixed positional layout**: element *n* means the same thing in every document of a version
5. **One document per signature**: bytes after the first complete CBOR item are refused

## Schema Versioning Strategy

- **v1** (`VERSION_V1`): text-keyed map, historical, decoded only. Deterministic but not
  §4.2.1 canonical — and it stays that way, because its bytes are already signed.
- **v2** (`VERSION_V2`): the 18-element array. What new documents carry.
- **New fields go last**, then the version is bumped. A decoder that does not know a version
  refuses the document rather than reading the prefix it recognises.
- **Shape must agree with the declared version**: v1 must be a map, v2 must be an array. A
  document that contradicts itself is not read against whichever shape happens to fit.

```rust
match payload_version(&signed_payload)? {
    VERSION_V1 => { /* map with text keys */ }
    VERSION_V2 => { /* array of V2_ELEMENT_COUNT */ }
    other => return Err(EncodeError::UnsupportedVersion(other)),
}
```

## Pitfalls

1. **Adding `#[derive(Serialize)]` to a payload type** — serde encodes a Rust struct as a CBOR
   *map* with text keys. That is the v1 shape and it is not what v2 is
2. **Passing a bare `Vec<u8>` to `serialize_element`** — serde routes it through
   `serialize_seq`, emitting an array of integers (major type 4). Wrap it so
   `serialize_bytes` is called
3. **Omitting an absent optional** — drops an element and silently shifts every later one.
   Write `null` in position; the decoder refuses any array whose count ≠ `V2_ELEMENT_COUNT`
4. **Accepting what ciborium accepts** — its reader will happily read a byte array where a byte
   string belongs. Strictness has to be written (and is, in `ByteBufVisitor`)
5. **Guessing at a version** — reading the fields you recognise and reporting the rest as absent.
   Refuse instead
6. **Re-encoding to compare** — verifying by re-encoding from the row's columns will drift on
   timestamp spelling, float width, or an unset column. Verify over the stored bytes
7. **Trusting a decoded enum code** — a code outside the table has no column value.
   `decode_payload` refuses it; don't re-derive a label by hand
8. **`f32` for coordinates, `String` for hashes, hex text for `device_hash`** — all break
   byte-equality. Coordinates are `f64`, altitude/accuracy are `f32` because that is what the
   `REAL` columns can show
9. **Nanosecond timestamps** — the column stores microseconds, so a nanosecond spelling cannot
   be reproduced from the row it came from (`canonical_timestamp` truncates, it does not round)
10. **Trailing bytes** — CBOR does not require a reader to notice them, and they are unsigned.
    Refused by `bytes_after_the_document_are_refused`

## Dependencies

```toml
[dependencies]
ciborium = "0.2"              # Pure Rust CBOR (RFC 8949)
ed25519-dalek = { version = "2.0", features = ["rand_core"] }
hex = "0.4"                   # Hex encoding for display/debug
```

## Testing Checklist

Run with `TMPDIR=/workspace/target/tmp` (the container's `/tmp` is noexec, so doctests fail to
run without it). Existing coverage lives in `app/src/provenance/{payload,encode}.rs`:

- [ ] **Golden bytes**: `version_two_bytes_are_pinned` — the encoder matches `V2_FIXTURE_HEX`
- [ ] **Determinism**: `version_two_encoding_is_deterministic`
- [ ] **Layout / major types**: `every_element_is_the_type_the_layout_names`,
      `a_version_two_document_is_an_array_of_the_declared_number_of_elements`
- [ ] **Byte strings**: `the_binary_fields_are_byte_strings_not_arrays_of_integers`
- [ ] **No maps**: `the_encoder_puts_no_maps_anywhere_in_a_payload`
- [ ] **Preferred float width**: `floats_use_the_shortest_form_that_keeps_the_value`
- [ ] **Arity, codes, trailing bytes refused**: see the spec's Testing section
- [ ] **Every field inside the bytes**: `every_field_of_version_two_is_inside_the_bytes` —
      mutate one field, the bytes must change
- [ ] **v1 unchanged**: `the_v1_encoder_is_untouched_by_the_new_version`,
      `a_version_one_signature_still_verifies_over_its_stored_bytes`
- [ ] **Code tables ↔ column labels**: `repo/tests/wire_types.rs` pins the labels against `pg_enum`

Any change to the layout **must** update `V2_FIXTURE_HEX` deliberately, as part of a version
bump — never to make a failing test pass.

## References

- **Spec**: `.knowledge/architecture/canonical-cbor-spec.md`
- **RFC 8949**: https://datatracker.ietf.org/doc/html/rfc8949
- **Deterministic encoding**: RFC 8949 Section 4.2.1
- **Architecture**: `.knowledge/architecture/provenance.md`
