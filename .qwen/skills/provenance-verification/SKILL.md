---
name: provenance-verification
description: Verify occurrence authenticity and node identity
source: custom
---

# Provenance & Signature Verification

## When to Use

- **Verifying occurrence authenticity** — When you need to confirm that an `occurrences` row genuinely originated from the claimed node
- **Working with origin_node_id vs reporting_node_id** — When distinguishing between who captured an observation vs. who relayed/wrote it
- **Implementing offline verification** — When verifying against local replicas without live CA calls
- **Signal node handling** — When dealing with light nodes that use `short_id` instead of full `node_id` on the wire
- **Multi-aggregator deduplication** — When multiple aggregators report the same signal-node observation

## Procedure

### 1. Node Identity (Self-Certifying)

```rust
// node_id is derived from signing public key
use sha2::{Sha256, Digest};

let node_id = Sha256::digest(&signing_public_key).to_vec();
```

**Key principle:** No registry lookup needed — anyone can recompute `SHA-256(signing_public_key)` to verify identity.

### 2. Key Generation (At Node Initialization)

```rust
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;

// Generate Ed25519 keypair
let mut csprng = OsRng;
let signing_key: SigningKey = SigningKey::generate(&mut csprng);
let verifying_key: VerifyingKey = signing_key.verifying();

// node_id = SHA-256(public_key)
let node_id = sha2::Sha256::digest(verifying_key.as_bytes()).to_vec();
```

**Critical:** Private key never leaves the node; only public key is shared.

### 3. Enrollment (CA Registration - One Time)

1. Node generates Ed25519 keypair locally
2. Node submits `signing_public_key` to CA
3. CA issues: `ca_credential = CA_sign(signing_public_key)`
4. Store in `nodes` table:
   - `signing_public_key`
   - `ca_credential`

**This is the ONLY CA involvement** — from here on, verification is offline.

### 4. Signing (At Capture Time)

Build canonical signed payload using **CBOR** (see `canonical-cbor-spec.md`):

```rust
use ciborium::into_writer;

#[derive(Serialize)]
struct ProvenancePayload {
    schema_version: u32,
    origin_node_id: Vec<u8>,
    device_hash: Vec<u8>,
    observed_at_node_local: String,  // ISO 8601 UTC
    rssi: i32,
    raw_payload_hex: String,
    location: Option<(f64, f64)>,     // Only if origin node determined it
}

let payload = ProvenancePayload { ... };
let mut buf = Vec::new();
into_writer(&payload, &mut buf).unwrap();

let signature = signing_key.sign(&buf);
```

**Store both verbatim:**
- `signed_payload` — the canonical CBOR bytes
- `signature` — the Ed25519 signature

**Do NOT reconstruct later** — stored bytes must match what was signed.

### 5. Verification (Any Time, Any Party)

```rust
use ed25519_dalek::{VerifyingKey, Signature, Verifier};

// 1. Look up node by node_id
let node = query!("SELECT signing_public_key, ca_credential, status FROM nodes WHERE node_id = $1", origin_node_id)
    .get_one(&pool)
    .await?;

// 2. Verify CA credential (once per node, cacheable)
verify_ca_credential(&node.ca_credential, &node.signing_public_key)?;

// 3. Check revocation
if node.status == "revoked" {
    return Err(RevokedNodeError);
}

// 4. Verify signature (per-occurrence)
let verifying_key = VerifyingKey::from_bytes(&node.signing_public_key)?;
let signature = Signature::from_bytes(&occurrence.signature);
verifying_key.verify(&occurrence.signed_payload, &signature)?;

// All checks passed — provenance confirmed
```

**No live CA call required** — only CA's root public key needed (single well-known value).

### 6. Multi-Aggregator Case

When multiple aggregators overhear the same signal-node ping:

1. Each aggregator independently verifies signal node's signature on raw `SignalPing`
2. Each computes the **same** deterministic `occurrence_id`
3. Each writes `occurrences` row with:
   - `origin_node_id` = signal node
   - `reporting_node_id` = itself (the aggregator)

**Result:** Identical `signed_payload`/`signature` across all rows → `ON CONFLICT DO NOTHING` dedup works cleanly.

## Pitfalls

### 1. Enrichment Fields Not Covered by Signature

**Problem:** Aggregator-added location/timestamp corrections for signal-node rows are **NOT** cryptographically bound to the signal node's original claim.

**Mitigation:** Trust aggregator's identity via `reporting_node_id` (aggregators are pre-registered with CA).

**TODO:** Consider adding second signature layer over enriched fields by `reporting_node_id`.

### 2. Revocation Propagation Delay

**Problem:** `nodes.status = 'revoked'` is local/gossiped, not instantly consistent across decentralized network.

**Mitigation:** Short-lived mTLS certs limit transport compromise window.

**TODO:** Consider signing key rotation on similar cadence as mTLS certs.

### 3. Canonical Encoding Drift

**Problem:** If canonicalization rules change across `schema_version`, reconstructed payloads won't match original signatures.

**Mitigation:** Store `signed_payload` and `signature` verbatim — never reconstruct from other columns.

**Requirement:** Use deterministic CBOR encoding (RFC 8949) via `ciborium` crate.

### 4. Signal Node Wire Budget

**Problem:** `SignalPing` payloads can't afford 32-byte `node_id`.

**Solution:** Use `short_id` (16-bit) for LoRa/Meshtastic transmission; aggregator resolves `short_id → node_id` using its full registry.

**Important:** `short_id` has no identity/trust role — it's purely for wire optimization.

### 5. Location Source Ambiguity

**Problem:** Not clear from the payload whether location came from `node_gps` or `node_fixed`.

**Solution:** The signed payload includes a boolean indicating which source was used:
- `(lat, lon)` included only if origin node determined it
- `null` if aggregator will add location later

## Key Data Model Fields

```sql
-- nodes table
signing_public_key BYTEA NOT NULL
ca_credential BYTEA NOT NULL
status TEXT NOT NULL DEFAULT 'active'  -- 'active' | 'revoked'

-- occurrences table
origin_node_id BYTEA NOT NULL          -- Who captured/signed
reporting_node_id BYTEA NOT NULL       -- Who wrote this row
signed_payload BYTEA NOT NULL          -- Verbatim canonical CBOR
signature BYTEA NOT NULL               -- Verbatim Ed25519 signature
```

## Dependencies

```toml
[dependencies]
ed25519-dalek = "2.0"
ciborium = "0.2"
sha2 = "0.10"
hex = "0.4"
```

## References

- **Canonical CBOR Spec:** [`canonical-cbor-spec.md`](../../.knowledge/architecture/canonical-cbor-spec.md)
- **Architecture Overview:** [`overview.md`](../../.knowledge/architecture/overview.md)
- **Data Model:** [`data-model.md`](../../.knowledge/architecture/data-model.md)
- **Data Flow:** [`data-flow.md`](../../.knowledge/architecture/data-flow.md)
