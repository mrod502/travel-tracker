# RFC 7250 Adaptation for CA Provenance Infrastructure

## Summary

RFC 7250 ("Using Raw Public Keys in TLS/DTLS") provides a standards-based framework for authenticating nodes using raw public keys instead of X.509 certificates. This document describes how RFC 7250 patterns can be adapted for our CA-based provenance scheme.

## Core Concepts from RFC 7250

### 1. Raw Public Key Authentication

RFC 7250 defines a `RawPublicKey` certificate type that uses the PKIX `SubjectPublicKeyInfo` (SPKI) structure directly in TLS handshakes, avoiding full certificate chains.

**SPKI Structure:**
```
SubjectPublicKeyInfo  ::=  SEQUENCE  {
     algorithm               AlgorithmIdentifier,
     subjectPublicKey        BIT STRING  }

AlgorithmIdentifier   ::=  SEQUENCE  {
     algorithm               OBJECT IDENTIFIER,
     parameters              ANY DEFINED BY algorithm OPTIONAL  }
```

### 2. Out-of-Band Binding Requirement

**Critical Security Principle (Section 6):**
> "The mechanism defined herein only provides authentication when an out-of-band mechanism is also used to bind the public key to the entity presenting the key."

RFC 7250 specifies several out-of-band binding methods:
- **DANE** (DNSSEC-validated TLSA records)
- **Pre-provisioned keys** (manufacturing-time configuration)
- **Out-of-band distribution** (secure channels, physical exchange)

### 3. Credential Type Negotiation

RFC 7250 defines TLS extensions for negotiating certificate types:
- `client_certificate_type` (type 0x40)
- `server_certificate_type` (type 0x41)

These allow peers to agree on using `RawPublicKey` vs `X.509` vs other formats.

## Adaptation to Our CA Infrastructure

### Current Implementation

Our `ca/` crate currently uses a simple credential format:
```rust
// Credential payload for signing:
[32-byte signing_public_key][8-byte issued_at][1-byte expiration_flag][8-byte expires_at?]

// Credential struct:
pub struct Credential {
    pub node_id: Vec<u8>,           // SHA-256(signing_public_key)
    pub signing_public_key: Vec<u8>, // 32 bytes
    pub ca_signature: Vec<u8>,      // 64 bytes Ed25519
    pub issued_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub issuer_id: Option<String>,
}
```

### Enhancement Proposal: SPKI-Wrapped Credentials

Optionally support RFC 7250-compliant SPKI encoding for the public key portion:

**SPKI Format for Ed25519 (RFC 8410):**
```
SubjectPublicKeyInfo ::= SEQUENCE {
  algorithm AlgorithmIdentifier ::= {
    algorithm OBJECT IDENTIFIER ::= 1.3.101.112  (id-Ed25519)
    parameters NULL
  }
  subjectPublicKey BIT STRING (32 bytes)
}
```

**Encoded Size:** ~46 bytes (vs 32 bytes raw)

**New Credential Structure:**
```rust
pub struct Credential {
    // Either raw or SPKI-encoded
    pub public_key_encoding: PublicKeyEncoding, 
    
    pub ca_signature: Vec<u8>,      // 64 bytes Ed25519
    pub issued_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub issuer_id: Option<String>,
}

pub enum PublicKeyEncoding {
    Raw(Vec<u8>),           // Current format (32 bytes)
    Spki(Vec<u8>),          // RFC 7250 SPKI (46 bytes for Ed25519)
}
```

### Benefits of SPKI Encoding

1. **Standards Compliance:** Interoperable with RFC 7250 TLS implementations
2. **Algorithm Identification:** SPKI includes OID, preventing key-type confusion
3. **TLS Integration:** SPKI can be directly used in mTLS handshakes
4. **Future-Proof:** Easy to add other key types (RSA, ECDSA)

### Trade-offs

| Aspect | Raw Format | SPKI Format |
|--------|-----------|-------------|
| Size | 32 bytes | 46 bytes (+44%) |
| Algorithm ID | Implicit (Ed25519 only) | Explicit (OID) |
| TLS Compatibility | Custom handling | Direct support |
| Dependencies | None | `der` or `pkix` crate |
| Complexity | Simple | ASN.1 parsing |

## Security Considerations

### 1. Trust Anchor Distribution

Per RFC 7250 Section 6, we must establish out-of-band binding. Our approach:
- **CA root public key** is the trust anchor
- Distributed via secure channels during initial node provisioning
- Can be embedded in firmware or exchanged during manufacturing

### 2. Credential Verification Flow

```
Node presents: SPKI(Ed25519_pubkey) + CA_signature
Verifier checks:
  1. SPKI decodes correctly (algorithm OID = 1.3.101.112)
  2. Extract public key from SPKI
  3. Verify CA_signature over [SPKI + metadata] using CA root public key
  4. Check expiration (issued_at <= now < expires_at)
  5. Check revocation (nodes.status field)
```

### 3. Man-in-the-Middle Protection

RFC 7250 warns about certificate-type negotiation attacks. Our mitigation:
- CA signature covers the entire credential (including key material)
- Attacker cannot substitute keys without CA re-signing
- TLS handshake authentication validates against CA-verified credential

## Implementation Recommendations

### Phase 1: SPKI Encoding Support

Add optional SPKI encoding to the `ca/` crate:

```rust
// ca/src/spki.rs
use der::{Encode, Decode};
use oid_registry::OID_ED25519; // 1.3.101.112

#[derive(Encode, Decode, Debug)]
pub struct SubjectPublicKeyInfo {
    pub algorithm: AlgorithmIdentifier,
    pub subject_public_key: der::bit_string::BitString,
}

pub fn encode_public_key_spki(public_key_bytes: &[u8; 32]) -> Result<Vec<u8>> {
    let spki = SubjectPublicKeyInfo {
        algorithm: AlgorithmIdentifier {
            algorithm: OID_ED25519,
            parameters: None, // Ed25519 has no parameters
        },
        subject_public_key: BitString::from_bytes(public_key_bytes)?,
    };
    spki.to_der().map_err(|e| CaError::Serialization(format!("{}", e)))
}

pub fn decode_public_key_spki(encoded: &[u8]) -> Result<[u8; 32]> {
    let spki = SubjectPublicKeyInfo::from_der(encoded)
        .map_err(|e| CaError::InvalidCredential(format!("SPKI decode failed: {}", e)))?;
    
    // Verify algorithm is Ed25519
    if spki.algorithm.algorithm != OID_ED25519 {
        return Err(CaError::InvalidKey(format!(
            "Expected Ed25519 OID, got {:?}", 
            spki.algorithm.algorithm
        )));
    }
    
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(spki.subject_public_key.as_bytes());
    Ok(key_bytes)
}
```

### Phase 2: Credential Format Negotiation

Add support for indicating encoding format in credentials:

```rust
// Add to Credential:
pub enum PublicKeyFormat {
    Raw,     // 0x00
    Spki,    // 0x01
}

pub struct Credential {
    pub public_key_format: PublicKeyFormat,
    pub public_key_bytes: Vec<u8>,  // Raw bytes or SPKI-encoded
    // ... rest of fields
}
```

### Phase 3: Protocol Integration

If implementing mTLS support, use RFC 7250 extensions:
- Advertise `RawPublicKey` support in `server_certificate_type` extension
- Present SPKI-encoded key in TLS `Certificate` payload
- Verify peer's key against CA credentials

## Dependencies

To implement SPKI support:
```toml
[dependencies]
der = "0.7"        # ASN.1 DER encoding/decoding
oid-registry = "0.7"  # OID constants (includes Ed25519)
```

## References

- [RFC 7250](https://datatracker.ietf.org/doc/rfc7250/) - Using Raw Public Keys in TLS/DTLS
- [RFC 8410](https://datatracker.ietf.org/doc/rfc8410/) - EdDSA in X.509 (defines Ed25519 OID)
- [RFC 5480](https://datatracker.ietf.org/doc/rfc5480/) - ECDSA SubjectPublicKeyInfo
- [RFC 5280](https://datatracker.ietf.org/doc/rfc5280/) - X.509 PKIX (SPKI definition)

## Appendix: Ed25519 SPKI Example

```
SubjectPublicKeyInfo ::= SEQUENCE {
  algorithm AlgorithmIdentifier ::= {
    algorithm OBJECT IDENTIFIER ::= 1.3.101.112
    parameters NULL
  }
  subjectPublicKey BIT STRING (length 36 bytes, 4 header + 32 key)
}

DER Encoding (hex):
302A                          # SEQUENCE, length 42
  3005                         # SEQUENCE, length 5
    0603 2B65 70              # OID 1.3.101.112 (Ed25519)
    0500                       # NULL
  0341 00                      # BIT STRING, length 65, 0 unused bits
    00                         # (32-byte public key follows)
    [32 bytes of public key]
```

Total: 46 bytes (vs 32 bytes for raw public key)
