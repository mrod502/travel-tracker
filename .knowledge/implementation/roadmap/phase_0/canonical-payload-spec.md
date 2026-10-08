# Canonical Payload Specification (Phase 0)

**Status:** ✅ **APPROVED** for Phase 0 Implementation  
**Date:** 2026-08-15  
**Reference:** [`../../architecture/canonical-cbor-spec.md`](../../architecture/canonical-cbor-spec.md)

---

## Overview

This document specifies the **exact byte layout** for the canonical signed payload used in Phase 0 Full Node implementation. It is a **concrete instantiation** of the architecture spec from [`canonical-cbor-spec.md`](../../architecture/canonical-cbor-spec.md), tailored for Phase 0 Bluetooth-only capture.

**Purpose:** Ensure that:
1. The same occurrence data always produces identical signed bytes
2. Any party can independently verify the signature offline
3. Schema evolution is possible via version field

---

## Design Decisions (Verified)

### 1. CBOR vs Alternatives

**Decision:** ✅ **CBOR via `ciborium` crate**

| Format | Deterministic? | Pure Rust? | Phase 0 Suitability |
|--------|---------------|------------|---------------------|
| **CBOR (ciborium)** | ✅ Yes (RFC 8949) | ✅ Yes | ✅ **SELECTED** |
| MessagePack | ⚠️ Partial | ✅ Yes | ❌ Less defined |
| JSON | ❌ No canonical form | ✅ Yes | ❌ Non-deterministic |
| bincode | ❌ Not for signing | ✅ Yes | ❌ Non-deterministic |

**Rationale:** Pure Rust requirement (no C/C++ deps) + deterministic encoding required for cryptographic signing.

### 2. Field Set

**Decision:** ✅ **12 fields in fixed order**

Fields are chosen to cover **what the origin node observed and asserted**, excluding aggregator-enriched fields (location from aggregator, sync-corrected timestamp).

### 3. Hash Format

**Decision:** ✅ **Raw bytes (32 bytes) for device_hash and origin_node_id**

**Rationale:** Compact storage, matches CBOR `bytes` type. For display/debugging, convert to hex string in application layer (not during signing).

### 4. Location Coverage

**Decision:** ✅ **Only include location if from origin node GPS/fixed**

Aggregator-added location is **NOT** covered by signature (trust aggregator via `reporting_node_id`). This matches the architecture spec.

---

## Field Specification

### Complete Field List (12 fields, exact order)

```
0.  schema_version      -- u16 (must be first for version detection)
1.  signal_type         -- u8 (0=bluetooth, 1=wifi, etc.)
2.  origin_node_id      -- bytes[32] (SHA-256 hash of signing public key)
3.  device_hash         -- bytes[32] (SHA-256 hash of device address)
4.  device_address      -- Optional bytes[6] (BLE MAC address)
5.  observed_at_node_local -- text (ISO 8601 UTC timestamp)
6.  rssi                -- i16 (signed RSSI value in dBm)
7.  tx_power            -- Optional i16 (transmit power if present)
8.  adv_type            -- Optional u8 (BLE advertisement type)
9.  location            -- Optional array[2] or null (lat, lon as f64)
10. signal_payload      -- Optional bytes (raw signal-specific data)
11. advertised_name     -- Optional text (device name from AD payload)
```

### CBOR Type Mappings

| Rust Type | CBOR Type | Major Type | Encoding Notes |
|-----------|-----------|------------|----------------|
| `u16` (schema_version) | uint | 0 (uint) | Minimal encoding |
| `u8` (signal_type) | uint | 0 (uint) | Byte value |
| `Vec<u8>` (origin_node_id) | bytes | 2 (bytes) | 32 bytes definite-length |
| `Vec<u8>` (device_hash) | bytes | 2 (bytes) | 32 bytes definite-length |
| `Option<Vec<u8>>` (device_address) | bytes or null | 2 or 0xF6 | 6 bytes if Some |
| `String` (timestamps, name) | text_string | 3 (text) | UTF-8, definite-length |
| `i16` (rssi) | nint or uint | 1 (nint) or 0 (uint) | Per CBOR signed int rules |
| `Option<i16>` (tx_power) | nint/uint or null | varies | CBOR null = 0xF6 |
| `Option<u8>` (adv_type) | uint or null | 0 or 0xF6 | Byte value or null |
| `Option<[f64; 2]>` (location) | array or null | 4 (array) or 0xF6 | Definite-length array |
| `Option<Vec<u8>>` (signal_payload) | bytes or null | 2 or 0xF6 | Raw signal data |
| `Option<String>` (advertised_name) | text_string or null | 3 or 0xF6 | UTF-8 or null |

---

## Rust Struct Definition

```rust
use serde::{Deserialize, Serialize};

/// Canonical payload for cryptographic signing
/// 
/// Field order is CRITICAL - must match spec exactly (0-11)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CanonicalPayload {
    /// Schema version (MUST be first field)
    pub schema_version: u16,
    
    /// Type of signal (0=bluetooth, 1=wifi, etc.)
    pub signal_type: u8,
    
    /// Origin node identity (32-byte SHA-256 hash)
    pub origin_node_id: Vec<u8>,
    
    /// Device pseudonymous ID (32-byte SHA-256 hash)
    pub device_hash: Vec<u8>,
    
    /// Raw MAC address (6 bytes for BLE)
    pub device_address: Option<Vec<u8>>,
    
    /// Node-local timestamp (ISO 8601 UTC)
    pub observed_at_node_local: String,
    
    /// RSSI value in dBm
    pub rssi: i16,
    
    /// TX power from AD payload (if present)
    pub tx_power: Option<i16>,
    
    /// BLE advertisement type (if Bluetooth)
    pub adv_type: Option<u8>,
    
    /// Location if from origin node (lat, lon)
    pub location: Option<[f64; 2]>,
    
    /// Raw signal-specific payload data
    pub signal_payload: Option<Vec<u8>>,
    
    /// Advertised device name (if present)
    pub advertised_name: Option<String>,
}
```

---

## Construction Example (Phase 0 Bluetooth)

```rust
use chrono::Utc;
use sha2::{Sha256, Digest};

fn build_canonical_payload(
    origin_node_id: &[u8],           // 32 bytes
    device_address: &[u8],           // 6 bytes (BLE MAC)
    rssi: i16,
    raw_payload: &[u8],              // Raw BLE advertisement
    advertised_name: Option<&str>,
) -> CanonicalPayload {
    // Compute device hash (SHA-256 of MAC address)
    let device_hash = {
        let mut hasher = Sha256::new();
        hasher.update(device_address);
        hasher.finalize().to_vec()
    };

    // Build signal-specific payload (Bluetooth)
    let mut ble_payload = serde_json::Map::new();
    ble_payload.insert("raw_payload_hex".to_string(), serde_json::json!(hex::encode(raw_payload)));
    ble_payload.insert("rssi".to_string(), serde_json::json!(rssi));
    
    if let Some(name) = advertised_name {
        ble_payload.insert("name".to_string(), serde_json::json!(name));
    }

    // Wrap in signal_type key
    let mut signal_payload = serde_json::Map::new();
    signal_payload.insert("ble".to_string(), serde_json::json!(ble_payload));

    // Convert to raw bytes for CBOR encoding
    let signal_payload_bytes = serde_json::to_vec(&signal_payload).unwrap();

    // Build canonical payload
    CanonicalPayload {
        schema_version: 1,
        signal_type: 0,  // Bluetooth
        origin_node_id: origin_node_id.to_vec(),
        device_hash,
        device_address: Some(device_address.to_vec()),
        observed_at_node_local: Utc::now().to_rfc3339(),  // ISO 8601 UTC
        rssi,
        tx_power: None,  // Phase 0: not extracted from AD payload
        adv_type: None,  // Phase 0: could extract from raw payload
        location: None,  // Phase 0: no GPS on signal nodes (Full Node would have this)
        signal_payload: Some(signal_payload_bytes),
        advertised_name: advertised_name.map(|s| s.to_string()),
    }
}
```

---

## Encoding Algorithm

### Step 1: Construct Struct

```rust
let payload = build_canonical_payload(...);
```

### Step 2: Serialize to CBOR

```rust
use ciborium::ser::into_writer;

let mut buffer = Vec::new();
into_writer(&payload, &mut buffer)
    .expect("Failed to serialize to CBOR");

// buffer now contains canonical CBOR bytes
let signed_payload = buffer;
```

**Determinism guarantee:** `ciborium` follows RFC 8949 deterministic encoding rules:
- Preferred serialization (shortest encoding)
- No indefinite lengths
- Fixed field order (struct, not map)

### Step 3: Sign the Buffer

```rust
use ed25519_dalek::{SigningKey, Signer};

let signature: ed25519_dalek::Signature = private_key.sign(&signed_payload);
let signature_bytes = signature.to_bytes();  // 64 bytes
```

### Step 4: Store in Database

```rust
sqlx::query!(
    r#"
    INSERT INTO occurrences (
        occurrence_id, signal_type, origin_node_id,
        device_address, device_hash, observed_at, observed_at_node_local,
        rssi, signal_payload, location_source,
        signed_payload, signature, schema_version, ingested_at
    ) VALUES (
        $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14
    )
    ON CONFLICT (occurrence_id, observed_at) DO NOTHING
    "#,
    occurrence_id,
    SignalType::Bluetooth,
    &origin_node_id,
    &device_address,
    &device_hash,
    observed_at,
    observed_at_node_local,
    rssi,
    signal_payload_json,  // Separate JSONB column for querying
    LocationSource::NodeGps,
    &signed_payload,      // BYTEA - canonical CBOR
    &signature_bytes,     // BYTEA - 64 bytes
    1i16,                 // schema_version
    Utc::now()
)
.execute(pool)
.await?;
```

---

## Verification Algorithm

### Given: An occurrence row from database

```rust
// 1. Load occurrence data
let row = sqlx::query!(
    "SELECT origin_node_id, signed_payload, signature FROM occurrences WHERE occurrence_id = $1",
    occurrence_id
)
.fetch_one(pool)
.await?;

// 2. Look up node's public key
let node_row = sqlx::query!(
    "SELECT signing_public_key FROM nodes WHERE node_id = $1",
    row.origin_node_id
)
.fetch_one(pool)
.await?;

let public_key_bytes = node_row.signing_public_key;  // 32 bytes
let public_key = ed25519_dalek::VerifyingKey::from_bytes(&public_key_bytes.into())
    .expect("Invalid public key");

// 3. Load signature
let signature_bytes: Vec<u8> = row.signature;
let signature = ed25519_dalek::Signature::try_from(signature_bytes.as_slice())
    .expect("Invalid signature length");

// 4. Load signed payload (CBOR bytes)
let signed_payload: Vec<u8> = row.signed_payload;

// 5. Verify signature
public_key.verify(&signed_payload, &signature)
    .expect("Signature verification failed!");

// 6. Optionally, decode CBOR to inspect (for debugging/auditing)
use ciborium::de::from_reader;
let payload: CanonicalPayload = from_reader(&signed_payload[..])
    .expect("Failed to decode CBOR");

println!("Verified occurrence from node {:?} at {}", 
    payload.origin_node_id, 
    payload.observed_at_node_local
);
```

---

## CBOR Encoding Examples

### Example 1: Simple Bluetooth Occurrence (No Optional Fields)

```rust
let payload = CanonicalPayload {
    schema_version: 1,
    signal_type: 0,
    origin_node_id: vec![0x01; 32],
    device_hash: vec![0x02; 32],
    device_address: Some(vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]),
    observed_at_node_local: "2026-08-15T10:30:45.123+00:00".to_string(),
    rssi: -65,
    tx_power: None,
    adv_type: None,
    location: None,
    signal_payload: Some(vec![0x03, 0x04, 0x05]),
    advertised_name: None,
};
```

**Expected CBOR Structure (illustrative):**

```
A3                            # Map(3) - actual count depends on Option fields
  01                          # schema_version: 1
  00                          # signal_type: 0
  58 20 010101...             # origin_node_id: 32 bytes
  58 20 020202...             # device_hash: 32 bytes
  46 AA BB CC DD EE FF        # device_address: 6 bytes
  78 1A 2026-08-15T...        # observed_at_node_local: ISO 8601 string
  21                          # rssi: -65 (CBOR negative int)
  F6                          # tx_power: null
  F6                          # adv_type: null
  F6                          # location: null
  43 03 04 05                 # signal_payload: 3 bytes
  F6                          # advertised_name: null
```

**Note:** This is illustrative. Actual output depends on `ciborium` encoding specifics.

### Example 2: Full Bluetooth Occurrence (All Optional Fields)

```rust
let payload = CanonicalPayload {
    schema_version: 1,
    signal_type: 0,
    origin_node_id: vec![0x01; 32],
    device_hash: vec![0x02; 32],
    device_address: Some(vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]),
    observed_at_node_local: "2026-08-15T10:30:45.123+00:00".to_string(),
    rssi: -65,
    tx_power: Some(2),
    adv_type: Some(0x04),  // ADV_IND
    location: Some([40.6892_f64, -74.0445_f64]),
    signal_payload: Some(vec![0x03, 0x04, 0x05]),
    advertised_name: Some("MyDevice".to_string()),
};
```

---

## Testing Requirements

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_encoding_deterministic() {
        let payload = create_test_payload();

        let mut buffers = Vec::new();
        for _ in 0..100 {
            let mut buffer = Vec::new();
            into_writer(&payload, &mut buffer).unwrap();
            buffers.push(buffer);
        }

        // All encodings MUST be identical
        assert!(buffers.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn test_round_trip() {
        let original = create_test_payload();
        let mut buffer = Vec::new();
        into_writer(&original, &mut buffer).unwrap();

        let decoded: CanonicalPayload = from_reader(&buffer[..]).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_signature_verification() {
        let (private_key, public_key) = generate_keypair();
        let payload = create_test_payload();

        let mut buffer = Vec::new();
        into_writer(&payload, &mut buffer).unwrap();

        let signature = private_key.sign(&buffer);
        let result = public_key.verify(&buffer, &signature);

        assert!(result.is_ok());
    }

    #[test]
    fn test_signature_fails_on_tampering() {
        let (private_key, public_key) = generate_keypair();
        let payload = create_test_payload();

        let mut buffer = Vec::new();
        into_writer(&payload, &mut buffer).unwrap();

        let signature = private_key.sign(&buffer);

        // Tamper with the payload
        buffer[10] ^= 0xFF;

        let result = public_key.verify(&buffer, &signature);
        assert!(result.is_err());
    }

    #[test]
    fn test_optional_fields_null_vs_value() {
        let with_none = CanonicalPayload {
            tx_power: None,
            ..create_test_payload()
        };

        let with_some = CanonicalPayload {
            tx_power: Some(2),
            ..create_test_payload()
        };

        let mut buffer_none = Vec::new();
        let mut buffer_some = Vec::new();

        into_writer(&with_none, &mut buffer_none).unwrap();
        into_writer(&with_some, &mut buffer_some).unwrap();

        // Must be different encodings
        assert_ne!(buffer_none, buffer_some);
    }

    #[test]
    fn test_schema_version_detection() {
        let payload = create_test_payload();
        let mut buffer = Vec::new();
        into_writer(&payload, &mut buffer).unwrap();

        // Peek at first field to detect version
        // (Implementation detail: would need custom CBOR decoder logic)
        let version = peek_schema_version(&buffer).unwrap();
        assert_eq!(version, 1);
    }
}
```

### Integration Tests

```rust
#[cfg(test)]
mod integration_tests {
    use super::*;

    #[tokio::test]
    async fn test_full_sign_verify_cycle() {
        let pool = create_test_pool().await;
        let node_identity = NodeIdentity::generate();

        // Create occurrence
        let occurrence = create_test_occurrence(&node_identity).await;

        // Store in database
        OccurrenceRepository::create(pool, &occurrence).await.unwrap();

        // Retrieve and verify
        let loaded = OccurrenceRepository::get_by_id(pool, &occurrence.occurrence_id)
            .await
            .unwrap();

        let public_key = load_node_public_key(pool, &loaded.origin_node_id).await;

        // Verify signature
        public_key.verify(&loaded.signed_payload, &ed25519_dalek::Signature::try_from(loaded.signature.as_slice()).unwrap())
            .unwrap();
    }

    #[tokio::test]
    async fn test_cross_platform_determinism() {
        // This test would be run on multiple platforms
        // and compared against known-good encoded bytes
        let payload = create_test_payload();
        let mut buffer = Vec::new();
        into_writer(&payload, &mut buffer).unwrap();

        // Compare against reference bytes (would need to be stored in repo)
        let reference_bytes = include_bytes!("reference_payload.cbor");
        assert_eq!(buffer, reference_bytes);
    }
}
```

---

## Schema Evolution Strategy

### Version 1 (Current)

All fields as defined above.

### Future Versions

To add new fields:

```rust
#[derive(Serialize, Deserialize)]
pub struct CanonicalPayloadV2 {
    pub schema_version: u16,  // = 2
    pub signal_type: u8,
    pub origin_node_id: Vec<u8>,
    pub device_hash: Vec<u8>,
    pub device_address: Option<Vec<u8>>,
    pub observed_at_node_local: String,
    pub rssi: i16,
    pub tx_power: Option<i16>,
    pub adv_type: Option<u8>,
    pub location: Option<[f64; 2]>,
    pub signal_payload: Option<Vec<u8>>,
    pub advertised_name: Option<String>,
    // New field added at end
    pub new_field_v2: Option<String>,
}
```

### Version Detection

```rust
fn decode_payload(buffer: &[u8]) -> Result<DecodedPayload> {
    // Peek at first field to determine version
    let version = peek_first_uint(buffer)?;

    match version {
        1 => {
            let payload: CanonicalPayloadV1 = from_reader(buffer)?;
            Ok(DecodedPayload::V1(payload))
        },
        2 => {
            let payload: CanonicalPayloadV2 = from_reader(buffer)?;
            Ok(DecodedPayload::V2(payload))
        },
        _ => Err(Error::UnsupportedVersion(version)),
    }
}
```

**Key principle:** New fields are added at the END, and are `Option<T>` to maintain backward compatibility.

---

## Dependencies

```toml
# app/Cargo.toml
[dependencies]
ciborium = "0.2"              # CBOR encoding/decoding
ed25519-dalek = "2.0"         # Ed25519 signatures
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
hex = "0.4"                   # Hex encoding
sha2 = "0.10"                 # SHA-256 for hash computation
chrono = { version = "0.4", features = ["serde"] }
```

---

## Security Considerations

### 1. Canonicalization Attacks

**Risk:** Non-deterministic encoding produces different bytes for same data.

**Mitigation:** Use `ciborium` which follows RFC 8949 deterministic encoding rules.

### 2. Type Confusion

**Risk:** Verifier uses different types than signer.

**Mitigation:** Document exact type mappings (this spec). Use serde for both encoding and decoding.

### 3. Schema Drift

**Risk:** Encoder/decoder version mismatch.

**Mitigation:** `schema_version` field, explicit version handling in decoders.

### 4. Key Storage

**Risk:** Private key exposure.

**Mitigation:** Never log private keys. Persist to encrypted storage in production.

### 5. Timing Attacks

**Risk:** Signature verification timing leaks information.

**Mitigation:** `ed25519-dalek` uses constant-time comparison.

---

## Implementation Checklist

### Pre-Implementation

- [x] ✅ CBOR format specified (this doc)
- [x] ✅ Field set finalized (12 fields)
- [x] ✅ CBOR type mappings documented
- [x] ✅ Dependencies identified

### Implementation

- [ ] Create `CanonicalPayload` struct
- [ ] Implement CBOR encoding (`encode_payload`)
- [ ] Implement CBOR decoding (`decode_payload`)
- [ ] Implement signing (`sign_payload`)
- [ ] Implement verification (`verify_signature`)
- [ ] Add unit tests (determinism, round-trip, signature)
- [ ] Add integration tests (full cycle, cross-platform)

### Post-Implementation

- [ ] Generate reference CBOR bytes for test vectors
- [ ] Document field order in code comments
- [ ] Add benchmarking (encoding speed)
- [ ] Fuzz test with random data

---

## References

- **Architecture Spec:** [`../../architecture/canonical-cbor-spec.md`](../../architecture/canonical-cbor-spec.md)
- **CBOR RFC 8949:** https://datatracker.ietf.org/doc/html/rfc8949
- **Deterministic Encoding:** https://datatracker.ietf.org/doc/html/rfc8949#section-4.2.1
- **ciborium crate:** https://crates.io/crates/ciborium
- **Ed25519 Dalek:** https://crates.io/crates/ed25519-dalek
- **Phase 0 Overview:** [`README.md`](./README.md)

---

**Last Updated:** 2026-08-15  
**Status:** ✅ **APPROVED** for Phase 0 Implementation
