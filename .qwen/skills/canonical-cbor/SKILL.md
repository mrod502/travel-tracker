---
name: canonical-cbor
description: Canonical CBOR encoding for signed occurrence payloads
source: custom
---

# Canonical CBOR Encoding for Travel System

## When to Use

- **Signing occurrence data** when creating signed provenance for signal detections (Bluetooth, WiFi, etc.)
- **Verifying occurrence signatures** when receiving relayed data from other nodes
- **Ensuring deterministic encoding** across platforms (Linux, macOS) for signature verification
- **Schema versioning** when evolving the signed payload structure over time

## Procedure

### 1. Define the Canonical Payload Struct

```rust
use serde::Serialize;

#[derive(Serialize)]
struct CanonicalPayload {
    pub schema_version: u16,           // Field 0: always first
    pub signal_type: u8,               // Field 1: signal type discriminator
    pub origin_node_id: Vec<u8>,       // Field 2: 32-byte SHA-256 hash
    pub device_hash: Vec<u8>,          // Field 3: 32-byte SHA-256 hash
    pub device_address: Option<Vec<u8>>, // Field 4: optional 6-byte MAC
    pub observed_at_node_local: String,  // Field 5: ISO 8601 UTC timestamp
    pub rssi: i16,                     // Field 6: signed RSSI value
    pub tx_power: Option<i16>,         // Field 7: optional transmit power
    pub adv_type: Option<u8>,          // Field 8: optional BLE ad type
    pub location: Option<[f64; 2]>,    // Field 9: optional [lat, lon]
    pub signal_payload: Option<Vec<u8>>, // Field 10: raw signal data
    pub advertised_name: Option<String>, // Field 11: optional device name
}
```

**Critical:** Field order MUST match exactly (0-11) to ensure determinism.

### 2. Serialize to CBOR

```rust
use ciborium::ser::into_writer;

let payload = CanonicalPayload {
    schema_version: 1,
    origin_node_id: node_id_bytes,  // 32 bytes
    device_hash: device_hash_bytes, // 32 bytes
    // ... other fields
};

let mut buffer = Vec::new();
into_writer(&payload, &mut buffer).expect("CBOR serialization failed");
// buffer now contains canonical CBOR bytes
```

### 3. Sign the CBOR Buffer

```rust
use ed25519_dalek::Signer;

let signature = private_key.sign(&buffer);
// signature is 64-byte Ed25519 signature
```

### 4. Store in Database

```sql
INSERT INTO occurrences (
    signed_payload,  -- BYTEA = canonical CBOR buffer
    signature        -- BYTEA = 64-byte Ed25519 signature
) VALUES ($1, $2);
```

### 5. Verify Signature (Verifier Side)

```rust
use ciborium::de::from_reader;
use ed25519_dalek::Verifier;

// 1. Load from database
let (signed_payload, signature_bytes) = load_occurrence(occurrence_id);

// 2. Verify schema version (peek at first bytes)
let version = decode_version(&signed_payload)?;
if version != 1 {
    return Err(UnsupportedVersion);
}

// 3. Verify Ed25519 signature
let public_key = load_node_public_key(origin_node_id)?;
let sig = ed25519_dalek::Signature::from_bytes(&signature_bytes);
public_key.verify(&signed_payload, &sig)?;

// 4. Optionally decode for inspection
let payload: CanonicalPayload = from_reader(&signed_payload[..])?;
```

## Type Mappings

| Rust Type | CBOR Type | Notes |
|-----------|-----------|-------|
| `u16` | `uint` | Minimal encoding (major type 0) |
| `u8` | `uint` | Byte value |
| `i16` | `nint` or `uint` | Per CBOR signed rules |
| `Vec<u8>` | `bytes` | Definite-length (major type 2) |
| `Option<Vec<u8>>` | `bytes` or `null` | CBOR null = 0xF6 |
| `String` | `text_string` | UTF-8, definite-length |
| `Option<[f64; 2]>` | `array` or `null` | Definite-length array |

## CBOR Deterministic Rules (RFC 8949)

1. **Preferred serialization**: Shortest encoding for integers/floats
2. **No indefinite lengths**: All arrays/maps specify length upfront
3. **Definite-length strings**: No streaming encoding
4. **Field ordering**: Use structs (not HashMaps) to preserve order

## Schema Versioning Strategy

- **Version field**: First field (`schema_version: u16`)
- **Forward compatibility**: New fields appended at end
- **Version detection**: Check `schema_version` before deserializing
- **Version upgrade**: Create new struct variant (e.g., `CanonicalPayloadV2`)

```rust
match schema_version {
    1 => { /* deserialize V1 */ },
    2 => { /* deserialize V2 */ },
    _ => return Err(UnsupportedVersion),
}
```

## Pitfalls

1. **Non-deterministic encoding**: Using `HashMap` instead of struct → field ordering varies
2. **Hex string vs bytes**: `device_hash` must be `Vec<u8>` (32 bytes), NOT hex string
3. **Timestamp format**: Must use ISO 8601 UTC format consistently
4. **Case inconsistency**: Hex values must be consistently lowercase or uppercase
5. **Type mismatch**: Verifier must use identical Rust types as signer
6. **Schema drift**: Missing `schema_version` check causes silent corruption
7. **Option handling**: `None` encodes as CBOR `null` (0xF6), not empty bytes
8. **Float precision**: Location coordinates must use `f64`, not `f32`
9. **Network byte order**: All multi-byte integers use network (big-endian) encoding per CBOR spec

## Dependencies

```toml
[dependencies]
ciborium = "0.2"              # Pure Rust CBOR (RFC 8949)
ed25519-dalek = "2.0"         # Ed25519 signatures
hex = "0.4"                   # Hex encoding for display/debug
```

## Testing Checklist

- [ ] **Determinism test**: Encode same payload 1000 times → all identical
- [ ] **Round-trip test**: Serialize → deserialize → equals original
- [ ] **Cross-platform test**: Encode on Linux, verify on macOS
- [ ] **Version detection**: Correctly reject unsupported versions
- [ ] **Signature test**: Valid signature passes, tampered data fails

## References

- **RFC 8949**: CBOR specification (https://datatracker.ietf.org/doc/html/rfc8949)
- **Deterministic encoding**: RFC 8949 Section 4.2.1
- **Architecture**: See `provenance.md` for system-wide context
