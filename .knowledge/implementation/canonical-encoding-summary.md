# Canonical Encoding Summary

**Date:** 2026-08-14  
**Status:** ✅ **COMPLETE**  
**Reference:** [`../architecture/canonical-cbor-spec.md`](../architecture/canonical-cbor-spec.md)

---

## Executive Summary

The canonical signed payload encoding has been **fully specified** using CBOR (RFC 8949). This resolves one of the three critical Phase 0 blockers.

### Key Decisions

1. **Format:** CBOR (Concise Binary Object Representation)
2. **Rust Crate:** `ciborium` (pure Rust, serde-compatible)
3. **Determinism:** RFC 8949 Section 4.2.1 Core Deterministic Encoding Requirements
4. **Schema Versioning:** First field is `schema_version: u16` for forward compatibility

---

## Why CBOR?

| Format | Deterministic? | Pure Rust? | Schema Evolution | Wire Size | Decision |
|--------|---------------|------------|------------------|-----------|----------|
| **CBOR (ciborium)** | ✅ Yes (RFC 8949) | ✅ Yes | ✅ Via schema_version | ✅ Compact | **SELECTED** |
| MessagePack | ⚠️ Partial | ✅ Yes | ❌ Limited | ✅ Compact | ❌ Less defined |
| JSON | ❌ No canonical form | ✅ Yes | ✅ Via schema_version | ❌ Verbose | ❌ Non-deterministic |
| Protobuf | ✅ Yes | ❌ C++ deps | ✅ Strong | ✅ Compact | ❌ Pure Rust req |
| bincode | ❌ Not for signing | ✅ Yes | ❌ No | ✅ Compact | ❌ Non-deterministic |

**Pure Rust requirement:** The project cannot accept C/C++ dependencies (protobuf crate rejected).

---

## Field Set (Signed Content)

The signed payload includes 12 fields in **exact order**:

```
0.  schema_version      -- u16 (must be first for version detection)
1.  origin_node_id      -- bytes[32] (SHA-256 hash)
2.  device_hash         -- text (64-char hex string)
3.  observed_at_node_local -- text (ISO 8601 UTC timestamp)
4.  rssi                -- i16 (signed RSSI value)
5.  raw_payload_hex     -- text (hex-encoded BLE advertisement)
6.  location            -- array[2] or null (lat, lon as f64)
7.  adv_type            -- u8 (BLE advertisement type)
8.  address_type        -- text (public/random/static/resolvable)
9.  advertised_name     -- text or null (optional BLE device name)
10. service_uuids       -- array of text or null (optional)
11. manufacturer_data   -- bytes or null (optional)
```

**Critical:** Field order is enforced by Rust struct layout, not HashMap ordering.

---

## Dependencies to Add

```toml
[dependencies]
ciborium = "0.2"              # Pure Rust CBOR serialization/deserialization
ed25519-dalek = "2.0"         # Ed25519 digital signatures (pure Rust)
hex = "0.4"                   # Hex encoding/decoding utilities
```

---

## Implementation Architecture

### Layer 1: Data Model (Rust Struct)

```rust
#[derive(Serialize, Deserialize)]
pub struct CanonicalPayload {
    pub schema_version: u16,
    pub origin_node_id: Vec<u8>,
    pub device_hash: String,
    pub observed_at_node_local: String,
    pub rssi: i16,
    pub raw_payload_hex: String,
    pub location: Option<[f64; 2]>,
    pub adv_type: u8,
    pub address_type: String,
    pub advertised_name: Option<String>,
    pub service_uuids: Option<Vec<String>>,
    pub manufacturer_data: Option<Vec<u8>>,
}
```

### Layer 2: Serialization (CBOR)

```rust
use ciborium::ser::into_writer;

pub fn encode_payload(payload: &CanonicalPayload) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    into_writer(payload, &mut buffer)?;  // Deterministic CBOR
    Ok(buffer)
}
```

### Layer 3: Signing (Ed25519)

```rust
use ed25519_dalek::Signer;

pub fn sign_payload(private_key: &SigningKey, payload_bytes: &[u8]) -> Signature {
    private_key.sign(payload_bytes)
}
```

### Layer 4: Storage (Database)

```rust
// Store both CBOR payload and signature
sqlx::query!(
    "INSERT INTO occurrences (signed_payload, signature, ...) VALUES ($1, $2, ...)",
    payload_bytes,    // BYTEA
    signature_bytes   // BYTEA (64 bytes)
)
```

### Layer 5: Verification (Any Node)

```rust
// 1. Load occurrence row
let (signed_payload, signature, origin_node_id) = load_occurrence(id);

// 2. Get public key from nodes table
let public_key = load_public_key(origin_node_id);

// 3. Verify signature
public_key.verify(&signed_payload, &signature)?;
```

---

## Separation of Concerns

The implementation maintains clear separation between layers:

```
┌─────────────────────────────────────────────────────┐
│ Application Layer (app/)                            │
│ - High-level business logic                         │
│ - BLE advertisement parsing                         │
│ - Node identity management                          │
└──────────────────┬──────────────────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────────────────┐
│ Serialization Layer (NEW: app/src/provenance/)      │
│ - CanonicalPayload struct                           │
│ - CBOR encoding/decoding                            │
│ - Signature generation/verification                 │
│ **This layer is format-agnostic to app layer**     │
└──────────────────┬──────────────────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────────────────┐
│ Storage Layer (db/)                                 │
│ - BYTEA columns for signed_payload and signature   │
│ - No knowledge of CBOR format                      │
│ - Just stores opaque bytes                         │
└─────────────────────────────────────────────────────┘
```

**Key principle:** The database schema does NOT need to change if we switch from CBOR to another format. The app layer handles all serialization details.

---

## Testing Strategy

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
            let buffer = encode_payload(&payload).unwrap();
            buffers.push(buffer);
        }
        
        // All encodings MUST be identical
        assert!(buffers.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn test_round_trip() {
        let original = create_test_payload();
        let encoded = encode_payload(&original).unwrap();
        let decoded = decode_payload(&encoded).unwrap();
        
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_signature_verification() {
        let (private_key, public_key) = generate_keypair();
        let payload = create_test_payload();
        
        let encoded = encode_payload(&payload).unwrap();
        let signature = sign_payload(&private_key, &encoded);
        
        let result = public_key.verify(&encoded, &signature);
        assert!(result.is_ok());
    }

    #[test]
    fn test_signature_fails_on_tampering() {
        let (private_key, public_key) = generate_keypair();
        let payload = create_test_payload();
        
        let mut encoded = encode_payload(&payload).unwrap();
        let signature = sign_payload(&private_key, &encoded);
        
        // Tamper with the payload
        encoded[10] ^= 0xFF;
        
        let result = public_key.verify(&encoded, &signature);
        assert!(result.is_err());
    }
}
```

### Integration Tests

- Test with real Postgres database
- Test cross-platform compatibility (Linux ↔ macOS)
- Test schema version detection (V1 vs V2 payloads)

---

## Migration Path

### Current State
- No provenance signing implemented
- `signed_payload` and `signature` columns exist but are unused

### Phase 0 Implementation

1. **Add dependencies** to `app/Cargo.toml`:
   ```toml
   ciborium = "0.2"
   ed25519-dalek = "2.0"
   hex = "0.4"
   ```

2. **Create provenance module** (`app/src/provenance/`):
   ```
   app/src/provenance/
   ├── mod.rs              # Public API
   ├── payload.rs          # CanonicalPayload struct
   ├── encode.rs           # CBOR serialization
   ├── sign.rs             # Ed25519 signing
   └── verify.rs           # Signature verification
   ```

3. **Implement signing at capture time**:
   ```rust
   // In BLE scanner event handler
   let payload = CanonicalPayload {
       schema_version: 1,
       origin_node_id: node_id,
       device_hash: compute_device_hash(&advertisement),
       observed_at_node_local: Utc::now().to_rfc3339(),
       rssi: advertisement.rssi,
       raw_payload_hex: hex::encode(&advertisement.data),
       location: node_location,
       adv_type: advertisement.adv_type,
       address_type: format_address_type(&advertisement.address),
       advertised_name: advertisement.name,
       service_uuids: advertisement.service_uuids,
       manufacturer_data: advertisement.manufacturer_data,
   };
   
   let signed_payload = encode_payload(&payload)?;
   let signature = sign_payload(&node_signing_key, &signed_payload);
   
   // Store in database
   insert_occurrence(signed_payload, signature, ...).await?;
   ```

4. **Implement verification** (for sync/debugging):
   ```rust
   // When syncing occurrences from peer node
   for occurrence in peer_occurrences {
       if verify_provenance(&occurrence).is_err() {
           log::warn!("Rejecting unverified occurrence");
           continue;
       }
       // Accept and store
   }
   ```

---

## Remaining Work

### Immediate (Before Phase 0 Exit)

- [x] ✅ Specify canonical encoding format (this doc)
- [ ] Implement `CanonicalPayload` struct
- [ ] Implement CBOR serialization/deserialization
- [ ] Implement Ed25519 signing at capture time
- [ ] Implement signature verification
- [ ] Add unit tests for determinism
- [ ] Add integration tests with database

### Future Enhancements

- [ ] Support schema version 2+ (if fields need to be added)
- [ ] Performance benchmarking (serialization speed)
- [ ] Wire size analysis (compare CBOR vs. alternatives in production)

---

## Related Documentation

- **Full Specification:** [`../architecture/canonical-cbor-spec.md`](../architecture/canonical-cbor-spec.md)
- **Provenance Architecture:** [`../architecture/provenance.md`](../architecture/provenance.md)
- **Research Topics:** [`../open-questions/research-topics.md`](../open-questions/research-topics.md)

---

**Last Updated:** 2026-08-14  
**Status:** ✅ **SPECIFIED** - Ready for Implementation
