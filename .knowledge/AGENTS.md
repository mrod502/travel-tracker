# Bluetooth Tracking Application — Knowledge Base Index

This knowledge base documents the architecture, design decisions, implementation status, and open questions for the distributed Bluetooth Low Energy (BLE) device tracking system.

## Quick Navigation

| Topic | Location | Description |
|-------|----------|-------------|
| **Architecture Overview** | [`architecture/overview.md`](architecture/overview.md) | High-level system design, node tiers, and distributed coordination model |
| **Data Model** | [`architecture/data-model.md`](architecture/data-model.md) | Database schema, occurrence records, device identity resolution |
| **Storage Strategy** | [`architecture/storage.md`](architecture/storage.md) | Postgres/PostGIS partitioning, H3 geo-indexing, retention policy |
| **Provenance & Security** | [`architecture/provenance.md`](architecture/provenance.md) | Node identity, signing/verification, mTLS trust model |
| **Data Flow** | [`architecture/data-flow.md`](architecture/data-flow.md) | End-to-end packet flow from BLE to storage, MQTT/LoRa transport |
| **Implementation Status** | [`implementation/status.md`](implementation/status.md) | What's done, what's missing, and where implementation diverges from plan |
| **Gap Analysis** | [`../GAP_ANALYSIS.md`](../GAP_ANALYSIS.md) | Per-component validation of the code against the claimed and intended end state |
| **Schema Analysis** | [`implementation/schema-divergence.md`](implementation/schema-divergence.md) | Database schema comparisons: planned vs. actual |
| **Repo Crate Analysis** | [`implementation/repo-divergence.md`](implementation/repo-divergence.md) | Rust model layer vs. schema (historic misalignment resolved) |
| **Provenance / CA / Revocation** | [`implementation/provenance-status.md`](implementation/provenance-status.md) | What is built vs. what is actually wired into the node |
| **Geo / H3 Alignment** | [`implementation/geo-h3-alignment.md`](implementation/geo-h3-alignment.md) | Signing the geo assertion, ownership derivation, workstreams WS0–WS6 |
| **H3 Geo-indexing** | [`technical/h3o-crate.md`](technical/h3o-crate.md) | Core types, patterns, and usage guidelines for the h3o Rust crate |
| **Research Topics** | [`open-questions/research-topics.md`](open-questions/research-topics.md) | All open questions, TODOs, and research areas |
| **Roadmap** | [`implementation/roadmap.md`](implementation/roadmap.md) | Phased implementation plan (Phase 0–6) |

**Current Phase**: Phase 0 — Single-Node Prototype (code-complete, never validated live)

**For Fast Agent Context**: See `.qwen/FAST_PROMPT.md` for concise system overview and current status.

---

## Blind Spots (Updated: September 2, 2026)

⚠️ **These are critical areas where we lack sufficient information to proceed effectively. Do not assume anything in these areas — flag them during implementation.**

### Critical Blockers for Phase 0

#### 0a. 🚨 Nothing in the Database Layer Has Ever Run
**Issue:** The dev container has no Postgres server, no PostGIS, no h3-pg, and no Docker
daemon. Seven migrations exist; not one has been executed.

**Consequence:** every schema-level claim in this knowledge base is inferred from reading SQL,
not observed. The most concrete instance: the only partitions created are July and August 2026,
so on today's date **every insert fails** with `no partition of relation "occurrences" found`.

**Action Required:** run the migrations against PostgreSQL 18 + PostGIS + h3-pg, create a
partition for the current month, and record the h3-pg version. Until then, no SQL change can be
verified here.

#### 0b. 🚨 `h3index` Wire Representation Is Unknown
**Issue:** `geo_cell_fine`/`geo_cell_macro` are read as `i64` and `owns_geo_cells` as
`Vec<i64>`, which assumes sqlx can decode h3-pg's `h3index` type that way. Nobody has checked.

**Risk:** if `h3index` is a distinct base type, the existing model fields break — not just new
code. Existing code works around it by casting the column to text on read.

**Action Required:** workstream WS0 in
[`implementation/geo-h3-alignment.md`](implementation/geo-h3-alignment.md). It gates WS2 and WS4.

#### 0c. ⚠️ Provenance Is Enforced on Write; Credential Trust Still Isn't
**Issue (rewritten 2026-10-08, [B11](../GAP_ANALYSIS.md) closed):** a node with
`[revocation].enabled = true` loads its CA's list at startup, verifies it against the
anchor in `[revocation].anchor_path`, refreshes it on a timer, and refuses to record an
occurrence the list revokes or can no longer vouch for. The empty checker described here
for months is gone; so is `enable_revocation_checking`.
Still open: no node verifies a CA **credential** — `verify_credential` runs only in the
`ca-verify` CLI — and there is no P2P transport, so `verify_received_occurrence` and
`should_allow_peer_connection` have no caller.

**Risk:** a node can prove who signed a row and that its CA has not revoked them, but not
that the CA issued the key it is signing with. Between two keys that both verify, the
`nodes` registry row is the only arbiter, and it is written by whoever ran `ca enroll`.

**Action Required:** see [`implementation/provenance-status.md`](implementation/provenance-status.md)
sections 3, 7, and 8 for the list of missing call sites (section 4's revocation half is
done); M14/M17/M18 in [`../GAP_ANALYSIS.md`](../GAP_ANALYSIS.md) track the credential
side.

---

#### 1. ✅ Canonical Signed Payload Encoding (RESOLVED)
**Status:** ✅ **SPECIFIED and implemented** —
[`architecture/canonical-cbor-spec.md`](architecture/canonical-cbor-spec.md), code in
`app/src/provenance/{payload,encode}.rs`

**Chosen format:** CBOR (RFC 8949) via `ciborium` crate (pure Rust)

**Key decisions:**
- Field order: Fixed struct ordering (schema_version first, then 11 fields)
- Encoding: Deterministic CBOR per RFC 8949 Section 4.2.1
- Timestamps: ISO 8601 UTC format
- Optional fields: CBOR null vs value
- Schema versioning: First field enables forward compatibility

**Open follow-on:** payload **v2**, signing the geo assertion. Not started — see
[`implementation/geo-h3-alignment.md`](implementation/geo-h3-alignment.md) D3.

**References:** [`architecture/provenance.md`](architecture/provenance.md), [`architecture/canonical-cbor-spec.md`](architecture/canonical-cbor-spec.md)

---

#### 2. ⚠️ h3-pg Extension API — Documented, Still Unexercised
**Status:** ⚠️ **Checked against PGXN documentation; never run against an installed extension**

**Findings:**
- Extension name: `h3` (not `h3_postgis` — no separate extension exists)
- Function name: `h3_latlng_to_cell()` (no underscore between "lat" and "lng")
- Signatures as documented: `h3_latlng_to_cell()`, `h3_cell_to_parent()`

**Why this is still a blind spot:** documentation is not execution. h3-pg v3 took lat/lng in the
opposite order and still returns a *valid* cell for transposed input, so the wrong version fails
silently rather than loudly. The migration encodes the v4 convention
(`point(ST_Y(...), ST_X(...))`, lat first). Nothing in Rust currently cross-checks the database's
answer, because until `repo::geo` existed there was no independent implementation to compare
against — that comparison is now possible and is not yet wired (WS4).

**References:** [`architecture/storage.md`](architecture/storage.md), [`technical/h3-pg-extension.md`](technical/h3-pg-extension.md), [`implementation/geo-h3-alignment.md`](implementation/geo-h3-alignment.md)

---

#### 3. ✅ Repo Model / Schema Misalignment (RESOLVED)
**Status:** ✅ **Resolved** — the models and the schema agree.

The schema was refactored from `bluetooth_occurrences` onto a unified `occurrences` table and
the repo crate onto a matching `Occurrence` model: `origin_node_id: Vec<u8>`,
`PostgisPoint` location, `signal_payload: serde_json::Value`, raw-byte `device_hash`, typed
enums, `signed_payload`/`signature`/`schema_version`, and the generated H3 columns.
`OccurrenceRelay` models the relay table. `h3o` is genuinely used via `repo::geo`.

**What remains is coverage, not shape:** no relay repository, no `sync_cursors` model,
`NodeRepository` cannot list peers or update `last_seen_at`, `ble_address_type` is defined but
unused, and the `h3index` decode assumption above. See
[`implementation/repo-divergence.md`](implementation/repo-divergence.md) R1–R8.

---

### Phase 3 Blockers

#### 4. Association Strength Composite Formula
**Issue:** No validated formula for scoring device associations. Current hypothesis weights geo/day diversity over raw co-occurrence count, but this is untested.

**Risk:** False positives (devices at busy transit stops) or false negatives (real associations missed).

**Action Required:** Generate synthetic known-paired vs known-incidental device data, prototype scoring formula, validate against synthetic ground truth.

**References:** [`open-questions/research-topics.md`](open-questions/research-topics.md)

---

#### 5. Identity Resolution Heuristics Validation
**Issue:** Fingerprint-match and temporal-adjacency heuristics assumed to work across address rotations, but varies by device/OS.

**Action Required:** Validate against real device behavior.

**References:** [`open-questions/research-topics.md`](open-questions/research-topics.md)

---

### Phase 4 Blockers

#### 6. SignalPing Truncated Hash Size
**Issue:** `SignalPing` wire format uses `device_hash_short` (truncated to ~10-12 bytes). The exact byte length for target density (~50 devices/sq mile, NYCMesh scale) has not been calculated against birthday bound collision risk.

**Risk:** Too short → collisions in high-density areas. Too long → exceeds LoRa payload budget (~200-256 bytes).

**Action Required:** Run birthday-bound calculation with target density numbers before implementing signal node firmware.

**References:** [`open-questions/research-topics.md`](open-questions/research-topics.md)

---

#### 7. Aggregator Enrichment Trust Boundary
**Issue:** When an aggregator adds location/timestamp correction for signal-node rows, those enrichment fields are NOT covered by the origin node's signature. They're only attributable via `reporting_node_id`.

**Risk:** Malicious aggregator could falsify enrichment data.

**Action Required:** Decide whether to add a second signature layer over enrichment fields, or accept this trust boundary.

**References:** [`architecture/provenance.md`](architecture/provenance.md), [`open-questions/research-topics.md`](open-questions/research-topics.md)

---

### Phase 5 Blockers

#### 8. Gossip Protocol Scaling
**Issue:** Full mesh gossip doesn't scale indefinitely. At what node count do we need a hub/relay pattern? No threshold determined.

**Risk:** Network performance degrades as node count grows.

**Action Required:** Load-test gossip protocol, establish scaling threshold, design hub/relay architecture before Phase 5.

**References:** [`open-questions/research-topics.md`](open-questions/research-topics.md)

---

#### 9. Re-Sharding Logic
**Issue:** When node ownership boundaries change (H3 macro cell reassignment), how do we handle existing data?

**Options:** Full recompute vs. H3 hierarchy walk vs. lazy migration.

**Action Required:** Finalize re-sharding strategy. Validate against load test.

**References:** [`open-questions/research-topics.md`](open-questions/research-topics.md)

---

#### 10. H3 Resolution Validation
**Issue:** Current assumption: res 6 for macro/ownership, res 9 for fine/indexing. Needs validation against real density data.

**Action Required:** Validate against 50-devices/sq-mile density target once real capture data exists from Phase 0.

**References:** [`architecture/storage.md`](architecture/storage.md), [`technical/h3o-crate.md`](technical/h3o-crate.md)

---

### Phase 6 Blockers

#### 11. Legal Review Boundary
**Issue:** Schema tracks devices only, not persons — but this needs formal legal review before any wider deployment.

**Risk:** Legal/regulatory exposure if collection practices don't comply with jurisdiction requirements.

**Action Required:** Engage legal counsel to review what's being collected, how it's stored, retention policies, and geographic scope before Phase 6.

**References:** [`open-questions/research-topics.md`](open-questions/research-topics.md)

---

#### 12. Signing-Key Rotation Policy
**Issue:** mTLS certs rotate short-lived by design, but signing keys (used for occurrence provenance) have no rotation cadence defined.

**Risk:** Long-lived signing keys increase exposure window if compromised.

**Action Required:** Define rotation cadence, key escrow/backup strategy for rotation, impact on historical verification.

**References:** [`open-questions/research-topics.md`](open-questions/research-topics.md)

---

### Unknown Unknowns

#### 13. Performance Characteristics at Scale
**Issue:** This system is untested at scale. Unknown performance characteristics, failure modes, and edge cases.

**Action Required:** Build monitoring and observability early. Design for rapid iteration based on real-world data from Phase 0.

---

## Document Maintenance

- When you update a diagram, update all referenced docs that mention it
- When you close an open question, update both the specific doc and [`open-questions/research-topics.md`](open-questions/research-topics.md)
- When you discover a new blind spot, add it to this file immediately — don't wait
- When implementing Phase N, verify all blind spots for that phase are resolved before proceeding

---

## Related External Resources

- **h3o Rust crate:** `cargo doc --package h3o --open`
- **h3-pg Postgres extension:** Verify installed version docs before implementing schema
- **PostGIS documentation:** https://postgis.net/documentation/
- **step-ca (PKI):** https://smallstep.com/docs/step-ca/
