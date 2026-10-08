# Open Research Topics

Consolidated from all planning docs. Items marked **[BLOCKING]** should be resolved before the phase that depends on them; others can proceed in parallel.

---

## Geo-Indexing (H3)

### ✅ Verify h3-pg Extension API (RESOLVED)

**Status:** ✅ **COMPLETE** - Verified against PGXN documentation (https://pgxn.org/dist/h3/docs/api.html)

**Findings:**
1. **Extension name:** The extension is `h3` (not `h3_postgis`). The PostGIS integration functions are included in the main `h3` extension.
2. **Function name correction:** `h3_latlng_to_cell()` (no underscore between "lat" and "lng"), NOT `h3_lat_lng_to_cell()`
3. **All other function signatures verified:**
   - `h3_latlng_to_cell(point/geometry/geography, resolution integer) -> h3index` ✅
   - `h3_cell_to_parent(cell h3index, resolution integer) -> h3index` ✅

**Fixes Applied:**
- Updated `db/src/migrations/202607312127_add_extensions.sql` - removed incorrect `h3_postgis` extension
- Updated `db/src/migrations/202607312147_create_bluetooth_occurrences.sql` - corrected function name to `h3_latlng_to_cell`

**Documentation:** See [`../technical/h3-pg-extension.md`](../technical/h3-pg-extension.md) for complete function reference.

**Blocked Phase:** None (resolved)

---

### [ ] Finalize H3 Resolution Levels
**Current assumption:** 
- Macro/ownership = res 6 (~36 km² cells)
- Fine/indexing = res 9 (~0.1 km² cells)

**Issue:** Needs validation against real density data.

**Action Required:** Validate res 6 (macro/ownership) and res 9 (fine/indexing) against the 50-devices/sq-mile density target once real capture data exists. Pull current `h3-pg` docs when doing this work.

**Blocked Phase:** Phase 5 (re-sharding logic depends on this)

---

### [ ] Automate LIST Sub-Partition Provisioning
**Issue:** Currently manual/example-only in the DDL. Rows for unprovisioned cells land in the `DEFAULT` partition (correct but unpruned).

**Action Required:** Automate `pg_partman` (or equivalent) config for partition creation whenever `nodes.owns_geo_cells` gains a new macro cell.

**Blocked Phase:** Phase 5 (re-sharding at scale)

---

### [ ] Re-Sharding Logic
**Issue:** When node ownership boundaries change (H3 macro cell reassignment), how do we handle existing data?

**Options:**
1. Full recompute (simple but expensive)
2. H3 hierarchy walk (complex but efficient)
3. Lazy migration (data moves on read)

**Action Required:** Finalize re-sharding strategy. Validate against load test.

**Blocked Phase:** Phase 5 (scaling to N nodes)

---

## Density & Scale Assumptions

### [ ] Truncated device_hash_short Byte Length
**Issue:** `SignalPing` wire format uses `device_hash_short` (truncated hash). The exact byte length for target density not calculated.

**Current recommendation:** 10-12 bytes for headroom at NYCMesh-scale density (up from bare-minimum 8).

**Action Required:** Run birthday-bound calculation with target device density per region. Validate against target: 50 devices/sq mile upper bound, ~250 existing Meshtastic longFast nodes in NYC for context.

**Blocked Phase:** Phase 4 (signal node firmware)

---

### [ ] Rate-Limit Threshold Validation
**Current target:** 10-20s max report frequency per device, per node.

**Issue:** Needs validation against real capture density.

**Action Required:** Measure real capture volume in Phase 0. May need per-node configurable threshold (dense vs sparse deployment areas).

**Blocked Phase:** Phase 0 (exit criteria)

---

### [ ] Retention Window Length
**Issue:** Raw-tier retention window (N days) not defined.

**Action Required:** Set from Phase 0 measured volume, not guessed up front.

**Blocked Phase:** Phase 6 (hardening/retention policy)

---

## Identity Resolution

### [ ] Validate Fingerprint-Match Heuristics
**Issue:** Fingerprint match (manufacturer/service data structure) assumed stable across address rotations, but varies by device/OS.

**Action Required:** Validate against real device behavior (address rotation intervals vary by device/OS).

**Blocked Phase:** Phase 3 (device identity resolution)

---

### [ ] Validate Temporal-Adjacency Heuristics
**Issue:** Temporal adjacency heuristic (old address stops, new address starts at same node/geo cell in tight window) needs validation.

**Action Required:** Test against real device behavior.

**Blocked Phase:** Phase 3 (device identity resolution)

---

### [ ] IRK Resolution Scope
**Issue:** IRK (Identity Resolving Key) resolution is cryptographic but likely out of scope unless devices are controlled/paired.

**Action Required:** Revisit if scope changes to include controlled devices.

**Blocked Phase:** None (likely out of scope)

---

## Association Detection

### [ ] association_strength Composite Formula **[BLOCKING]**
**Issue:** No validated formula for scoring device associations.

**Current hypothesis:** Weight geo/day diversity higher than raw co-occurrence count (two devices near busy transit stop vs. real association).

**Action Required:** Prototype against synthetic known-paired vs known-incidental device data. Generate synthetic ground truth, validate scoring formula before trusting on real data.

**Blocked Phase:** Phase 3 (association detection - BLOCKING)

---

### [ ] Evaluate ST_ClusterDBSCAN vs. Windowed Joins
**Issue:** Hand-rolled windowed joins for co-occurrence detection. Could use PostGIS `ST_ClusterDBSCAN` instead.

**Action Required:** Evaluate `ST_ClusterDBSCAN` (PostGIS spatial clustering) as alternative/complement to hand-rolled logic.

**Blocked Phase:** Phase 3 (implementation choice)

---

## Networking / Coordination

### [ ] Gossip Protocol Subscription Scaling
**Issue:** Full mesh gossip subscription doesn't scale indefinitely.

**Question:** At what node count do we need hub/relay pattern?

**Action Required:** Load-test gossip protocol. Establish scaling threshold (currently assumed ~few dozen nodes). Design hub/relay architecture before Phase 5.

**Blocked Phase:** Phase 5 (scaling to N nodes)

---

### [ ] Fallback for Signal-Node Cluster Losing All Aggregators
**Issue:** What happens if a signal-node cluster loses all aggregators in range?

**Options:**
1. Accepted coverage gap
2. Require redundant aggregator placement

**Action Required:** Decide fallback behavior. Design redundancy requirements if option 2.

**Blocked Phase:** Phase 4 (signal/aggregator tier)

---

### [ ] Bandwidth Budgeting
**Issue:** Bandwidth budgeting for raw vs. batched/compressed upload on constrained links.

**Action Required:** Calculate payload sizes. Design compression strategy.

**Blocked Phase:** Phase 1 (sync protocol)

---

## Security / Legal

### [ ] Formal Legal Review **[BLOCKING for Phase 6]**
**Issue:** Schema tracks devices only, not persons — but needs formal legal review before wider deployment.

**Risk:** Legal/regulatory exposure if collection practices don't comply with jurisdiction requirements.

**Action Required:** Engage legal counsel to review what's being collected, how it's stored, retention policies, and geographic scope before Phase 6.

**Blocked Phase:** Phase 6 (hardening - BLOCKING for production)

---

### [ ] CA Operational Model at Scale
**Issue:** CA-based node bootstrapping (step-ca vs. Vault PKI) not designed.

**Action Required:** Choose PKI solution. Design cert rotation cadence.

**Blocked Phase:** Phase 5 (scaling to N nodes)

---

### [ ] Signing-Key Rotation Cadence
**Issue:** mTLS certs rotate short-lived by design, but signing keys (used for occurrence provenance) have no rotation cadence defined.

**Risk:** Long-lived signing keys increase exposure window if compromised.

**Action Required:** Define rotation cadence, key escrow/backup strategy for rotation, impact on historical verification.

**Blocked Phase:** None (should be resolved before Phase 6)

---

## Provenance / Signature Verification

### [x] Canonical Encoding Byte Layout — **RESOLVED and implemented**
**Was:** `provenance.md` stated the field *set* but not the byte-exact canonical encoding.

**Resolution:** CBOR (RFC 8949) via the `ciborium` crate, implemented in
`app/src/provenance/{payload,encode}.rs` with determinism and round-trip tests.

- Field order: fixed struct ordering, `schema_version` (u16) first, then 11 fields
- Determinism: required by RFC 8949 §4.2.1
- Timestamps: ISO 8601 UTC text
- Optional fields: CBOR null (`0xF6`) vs value
- Versioning: version is the first field; future versions append only

**Note on the field list quoted in the original question:** it listed `raw_payload_hex`, which
does not exist as a column or as a payload field. Signal-specific detail rides in the JSONB
`signal_payload` column, and the signed payload's twelfth field is `advertised_name`. The
authoritative field list is
[`../architecture/canonical-cbor-spec.md`](../architecture/canonical-cbor-spec.md) and
`app/src/provenance/payload.rs`.

**Still open in this area:** payload **v2**, which would sign the geo assertion
(`geo_cell_fine`/`geo_cell_macro`, accuracy, altitude, location origin and fix age). Not
started; see [`../implementation/geo-h3-alignment.md`](../implementation/geo-h3-alignment.md) D3.

---

### [ ] Aggregator Enrichment Signature Layer
**Issue:** Aggregator-added location/timestamp for signal-node rows is NOT covered by origin node's signature.

**Options:**
1. Accept current trust boundary (trust aggregator's `reporting_node_id`)
2. Add second signature layer (aggregator signs enriched fields)

**Action Required:** Decide whether aggregator enrichment should get its own signature layer to close the "enrichment isn't covered" gap.

**Blocked Phase:** None (design choice, can be added later)

---

## Storage / Retention

### [x] pg_partman Configuration — **creation RESOLVED; rollup/drop cadence still open**
**Issue:** No automation for monthly partition creation and rollup/drop cadence.

**Concrete consequence as of 2026-09-02:** the only partitions created by the migrations are
`occurrences_2026_07` and `occurrences_2026_08`. Nothing covers September 2026 onward, so
running the migrations today and starting a node produces
`no partition of relation "occurrences" found for row` on every write.

**Resolved 2026-09-14 (creation):** `202609021200` adds
`ensure_occurrence_partitions(months_ahead)` and opens 16 months; `db up` re-extends the horizon
on every deploy. `202609141353` adds `ensure_occurrence_partition(ts)`, which
`OccurrenceRepository::create` calls when an insert is refused for want of a partition, so a node
that outruns the horizon — or takes a row dated outside it — repairs the month and retries rather
than losing the write. Deliberately no DEFAULT partition, and the on-demand path refuses a
timestamp more than 12 months back / 24 forward so a bad row cannot fill the catalog.

**Still open:** rollup into `occurrence_rollups` and the drop of aged partitions. Retention is
intentionally a separate, explicit operation — nothing here deletes data.

**Action Required:** schedule `ensure_occurrence_partitions` for installs that run past the horizon
without redeploying (pg_cron, systemd timer), then decide the rollup and drop policy.

**Blocked Phase:** Phase 6 (hardening/retention policy)

---

## Unknown Unknowns

### [ ] What We Don't Know Yet
**Issue:** This system is untested at scale. There are unknown performance characteristics, failure modes, and edge cases.

**Action Required:** Build monitoring and observability early. Design for rapid iteration based on real-world data.

**Blocked Phase:** None (ongoing throughout all phases)

---

## Priority Summary

### Blockers for Phase 0 (Must Resolve Now)
1. ✅ h3-pg extension API verification (RESOLVED - see above)
2. ✅ Canonical encoding byte layout for provenance (RESOLVED - see above)
3. Repo model / schema alignment (see `implementation/repo-divergence.md`)

### Blockers for Phase 3
4. Association strength composite formula (synthetic data validation required)
5. Identity resolution heuristics validation

### Blockers for Phase 4
6. SignalPing truncated hash byte length (birthday-bound calculation)
7. Aggregator fallback behavior design

### Blockers for Phase 5
8. Gossip protocol scaling threshold
9. Re-sharding logic for ownership changes
10. H3 resolution validation against real density

### Blockers for Phase 6
11. Legal review
12. Signing-key rotation policy
13. Retention window length (based on Phase 0-5 volume)

---

## References

- **Architecture:** [`../architecture/overview.md`](../architecture/overview.md)
- **Data Model:** [`../architecture/data-model.md`](../architecture/data-model.md)
- **Storage:** [`../architecture/storage.md`](../architecture/storage.md)
- **Provenance:** [`../architecture/provenance.md`](../architecture/provenance.md)
- **Implementation Status:** [`../implementation/status.md`](../implementation/status.md)
- **Roadmap:** [`../implementation/roadmap.md`](../implementation/roadmap.md)
- **Blind Spots:** [`../AGENTS.md#blind-spots`](../AGENTS.md#blind-spots)
