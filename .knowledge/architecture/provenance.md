# Provenance & Signature Verification

**Requirement:** Any party — not just the relaying aggregator — must be able to independently verify that a stored `occurrences` row genuinely originated from the node it claims to, **without a live call to the CA at verification time.**

## Design Principle

Every node, regardless of tier, holds an **Ed25519 signing keypair**, separate from any mTLS transport credential.

| Node tier | Has mTLS cert? | Has signing keypair? |
|-----------|----------------|----------------------|
| Full | Yes | Yes |
| Aggregator | Yes | Yes |
| Light *(future)* | Yes | Yes |
| Signal | No (no TLS over LoRa) | Yes |

**Separation matters:**
- mTLS secures a *connection*
- Signing key secures a *claim about a single observation*, independent of transport or hops

---

## Node Identity

`node_id` is **not** an independently allocated value:

- `node_id` = SHA-256(signing_public_key)
- Makes node identity **self-certifying**
- Any party can recompute `sha256(signing_public_key)` and confirm it matches the claimed `node_id`
- No separate registry lookup needed to trust that ID and key belong together

**Other identity fields:**
- `short_id` (16-bit) — LoRa/Meshtastic wire-budget optimization for signal nodes only
  - `SignalPing` payloads can't afford 32-byte `node_id`
  - `short_id → node_id` resolution happens at receiving aggregator (holds full registry)
  - Not used for identity/trust purposes anywhere else
- `display_name` — cosmetic field (e.g., 'brooklyn-rooftop-3') for logs/UI
  - Carries no identity or trust weight

---

## Enrollment (Once, at CA Registration)

1. **Node generates Ed25519 keypair locally** — private key never leaves the node
2. **Node submits public key to CA**
3. **CA issues credential:**
   ```
   ca_credential = CA_sign(signing_public_key)
   ```
   (node_id needs no separate assertion — already deterministically derived from key)
4. **Store in nodes table:**
   - `signing_public_key`
   - `ca_credential`
5. **Propagate** to rest of network through normal node-registry sync (NOT re-fetched from CA at verify time)

**This is the only point at which the CA is involved.** From here on, verification is entirely offline against locally cached data.

---

## Signing (At Capture Time, Every Occurrence)

Origin node builds a **canonical signed payload** — fixed, deterministic byte encoding of provenance-critical fields — and signs it:

```
signed_payload = canonical_encode(
    origin_node_id,
    device_hash,
    observed_at_node_local,     -- pre-correction (what origin node actually measured)
    rssi,
    raw_payload_hex,
    location_source == 'node_gps' or 'node_fixed'
        ? (lat, lon)            -- included only if origin node itself determined location
        : null,                 -- aggregator-added location is NOT covered
    schema_version
)

signature = Ed25519_sign(origin_node_private_key, signed_payload)
```

**Both `signed_payload` and `signature` stored verbatim** on the `occurrences` row:
- Not reconstructed later from other columns
- Avoids ambiguity about canonicalization rules drifting across schema_version changes
- Verifier never has to guess at canonicalization

**Important scope boundary:** The signature covers what the *origin* node observed and asserted. It does **NOT** cover fields an aggregator adds during enrichment:
- Aggregator-assigned location (for `aggregator_fixed` rows)
- `observed_at` sync-correction
- `ingested_at`

These fields are attributable to the *reporting* node, not the origin node, and are **not cryptographically guaranteed** — this is a known, deliberate boundary.

---

## Verification (By Any Party, Any Time)

Given an `occurrences` row:

1. **Look up node:** `nodes` WHERE `node_id = occurrence.origin_node_id`
2. **Verify CA credential** (once per node, cacheable):
   - Check `nodes.ca_credential` against CA's known root public key
   - Validates node's signing key was legitimately issued
3. **Verify signature** (per-occurrence):
   - Verify `occurrence.signature` against `occurrence.signed_payload`
   - Using `nodes.signing_public_key`
4. **Check revocation status:**
   - Verify `nodes.status != 'revoked'`
5. **If all checks pass:** Row confirmed to have originated from `origin_node_id`, as claimed, regardless of which node(s) relayed or wrote it.

**No network call required for step 3**
**Step 2 only requires CA's root public key** (single well-known value, not live lookup)
**Works fully offline against local replica**

---

## Multi-Aggregator Relay Case

When multiple aggregators overhear the same signal-node ping (see `architecture/overview.md`):

**Each aggregator independently:**
1. Verifies signal node's signature on raw `SignalPing`
2. Computes the **same** deterministic `occurrence_id`
3. Writes `occurrences` row with:
   - `origin_node_id` = signal node
   - `reporting_node_id` = itself

**Result:** Because `signed_payload`/`signature` are the signal node's own, both aggregators' rows carry **identical** signed content.

- `ON CONFLICT DO NOTHING` dedup works cleanly
- Independent provenance verification composes cleanly
- Nothing aggregator-specific baked into what's actually signed

---

## `origin_node_id` vs. `reporting_node_id`

**`origin_node_id`:**
- Who captured and signed the observation
- What provenance verification checks
- Trust boundary for the observation itself

**`reporting_node_id`:**
- Who wrote this specific row to this specific database
- Equals `origin_node_id` for full-node direct captures
- The relaying aggregator for signal-node-sourced rows
- Trust boundary for enrichment fields (aggregator-added location/timestamp)

**Both kept because they answer different questions:**
- "Who do I trust this observation came from?" → `origin_node_id`
- "Who do I ask if this row looks wrong / where did it enter my local copy?" → `reporting_node_id`

---

## Limitations / Explicit Non-Goals

### 1. Enrichment Fields Not Covered by Signature

**Problem:** Aggregator's added location/timestamp correction for signal-node rows is trusted based on aggregator's own node identity (visible via `reporting_node_id`), not cryptographically bound to signal node's original claim.

**Mitigation:** Trust model assumes aggregators are pre-registered with CA, so their identity is verifiable.

**Open question:** Should aggregator enrichment get its own signature layer (second signature over enriched fields, by `reporting_node_id`) to close this gap?

### 2. Revocation Propagation Delay

**Problem:** `nodes.status = 'revoked'` is local/gossiped flag, not instantaneously consistent across decentralized, sometimes-partitioned network.

**Mitigation:** Short-lived mTLS certs limit transport compromise window; signing keys don't have equivalent policy yet.

**Open question:** Should signing keys rotate on similar cadence as mTLS certs?

### 3. Canonical Encoding (RESOLVED)

**Status:** ✅ **SPECIFIED** - See [`canonical-cbor-spec.md`](canonical-cbor-spec.md)

**Chosen format:** CBOR (RFC 8949) via `ciborium` crate (pure Rust)

**Key decisions:**
- Field order: Fixed struct ordering (not HashMap)
- Schema versioning: First field is `schema_version` for forward compatibility
- All timestamps: ISO 8601 UTC format
- All hex strings: Lowercase (consistent)
- Determinism: Enforced by RFC 8949 deterministic encoding rules

**Dependencies:** `ciborium = "0.2"`, `ed25519-dalek = "2.0"`, `hex = "0.4"`

---

## Open Questions / TODOs

- [ ] **Decide signing-key rotation cadence** — currently unspecified; mTLS certs rotate short-lived, signing keys don't yet have equivalent policy
- [ ] **Decide whether aggregator enrichment should get its own signature layer** (second signature over enriched fields, by `reporting_node_id`) to close the "enrichment isn't covered" gap
- [x] **Canonical encoding specified** — See [`canonical-cbor-spec.md`](canonical-cbor-spec.md) for complete specification

---

## References

- **Architecture:** [`overview.md`](overview.md)
- **Data Model:** [`data-model.md`](data-model.md)
- **Data Flow:** [`data-flow.md`](data-flow.md)
- **Research Topics:** [`../open-questions/research-topics.md`](../open-questions/research-topics.md)
