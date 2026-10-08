# Implementation Roadmap

Phased implementation plan to validate assumptions before building complexity.

**Last updated:** September 2, 2026. Checkbox states reflect the working tree, not intent.

---

## Phase 0 — Single-Node Prototype

**Goal:** Nail the data model and local API before any networking complexity.

### Deliverables
- [x] bt_mon library (BLE scanning)
- [x] Database schema (migrations created — **never executed**)
- [x] App binary (scanning → storage flow)
- [x] Provenance signing integration (CBOR encode + sign on capture)
- [x] Rate limiting (per-device, 15 s default)
- [x] Position acquisition (fixed / GPS / mock / none)
- [x] Layered configuration (flag > file > env > default)
- [x] CA enrollment for the local node — **operator-driven only**: `app ca ca-init` then `app ca ca-enroll`
- [ ] Clock discipline beyond "single system clock" (NTP discipline, measured drift)
- [ ] Measure real capture volume
- [ ] Validate rate-limit threshold (10-20s)
- [ ] Create a partition for the current month (without it, every insert fails)

### Exit Criteria
Can query: "what did this node see in the last hour" and "where."
Occurrences include `signed_payload` and `signature` fields.

**Status against exit criteria:** the second half is met in code; neither half has been
demonstrated, because nothing has run against a database or a radio.

### Blockers
- [x] Canonical signed payload encoding (SPECIFIED and implemented — `canonical-cbor-spec.md`)
- [x] Repo model alignment (models now match the schema — see `repo-divergence.md`)
- [ ] H3 extension function verification (schema not tested against an installed version)
- [ ] **`h3index` wire representation** — unconfirmed whether sqlx can decode it as `i64` (WS0); gates WS2/WS4
- [ ] **Partition provisioning** — no partition covers September 2026 or later
- [ ] **Migrations have never been run** against PostgreSQL 18 + PostGIS + h3-pg
- [ ] **CA root key configuration** — needed before verification can be wired

---

## Phase 1 — Two-Node Sync

**Goal:** Prove replication before scaling to N nodes.

### Deliverables
- [ ] MQTT client integration
- [ ] Application-level batched sync protocol
- [ ] `ON CONFLICT DO NOTHING` dedup validation
- [ ] Simulated partition test (kill connectivity, keep scanning, reconnect)

### Exit Criteria
Node A and node B converge to same dataset after partition/reconnect cycle.

### Dependencies
- Phase 0 complete
- MQTT broker setup

### Timeline Estimate
2-3 weeks

---

## Phase 0.5 — CA Infrastructure (BLOCKER for Production)

**Goal:** Establish trust anchor and verification workflow before production deployment.

### Deliverables
- [x] **CA library crate** (`ca/` with credential issuance and verification)
- [x] **CA CLI subcommands** (`ca-init`, `ca-enroll`, `ca-verify`, `ca-info`)
- [x] **CA credential verification** (verify node credentials against CA root key)
- [x] **Revocation scheme design** (RSL-based with RFC 7250 compatibility)
- [x] **Revocation data structures** (`RevokedNode`, `RevocationStatusList`, `InMemoryRslChecker`)
- [x] **Policy evaluation framework** (`DataRecordingPolicy`, `ConnectionPolicy`)
- [ ] **CA enrollment protocol** (complete integration with node startup)
- [ ] **Credential issuance API** (submit public key, receive CA-signed credential)
- [ ] **Revocation checking integration** (check `nodes.status != 'revoked'` before verification)
- [ ] **Offline verification workflow** (complete: lookup node → verify CA cred → verify sig → check revocation)
- [ ] **CA root key configuration** (store CA public key in node config)
- [ ] **Credential distribution** (propagate ca_credential via node sync - for Phase 1+)

### Exit Criteria
Any party can verify an occurrence's authenticity without live CA access:
1. Load occurrence's `signed_payload` and `signature`
2. Lookup node's `signing_public_key` and `ca_credential` from local DB
3. Verify `ca_credential` against CA root key (cached after first check)
4. Verify `signature` against `signed_payload` using `signing_public_key`
5. Check node is not revoked
6. All checks pass → occurrence authentic

### Dependencies
- Phase 0 signing integration complete
- CA tooling selected (step-ca vs. custom)

### Timeline Estimate
2-4 weeks (depends on CA complexity)

### Notes
- **What "done" means in the list above:** the checked items are done *as library code plus a CLI*.
  `ca-verify` verifies a credential because an operator asked it to; the node never does.
- **Revocation scheme:** core library implemented in the `ca` crate. The publication side became real
  on 2026-09-14: `ca-generate-rsl` numbers, signs and stores every list in `revocation_status_lists`,
  so the anti-replay sequence outlives the process that issued it, and a checker holding no list, an
  expired list, or one older than its staleness bound answers `Unknown` instead of `Valid`
  (`ca/tests/rsl_persistence.rs`). What remains is the consumption side — `FullNode::new` still builds
  an **empty** `InMemoryRslChecker`, `enable_revocation_checking` only logs, and nothing calls
  `verify_rsl` when a stored list is loaded, so no runtime rejection is possible yet. See
  `.knowledge/architecture/specifications/revocation-scheme.md` and
  [`provenance-status.md`](provenance-status.md).
- **BLOCKER for production:** credentials are issued and verifiable, but nothing verifies them
  on the receive path, so the trust anchor is currently decorative.
- **BLOCKER for Phase 1:** node sync must propagate CA credentials, and there is no transport.
- **Security critical:** this is the foundation of the entire trust model.

---

## Phase 2 — Federated Query Layer

**Goal:** Answer "show me activity across the whole network" without full replication.

### Deliverables
- [ ] Federated query API (fan-out to multiple nodes)
- [ ] Result merging/deduplication
- [ ] Query routing (which node owns which geo cells)

### Exit Criteria
A query against the network API returns correct merged results from ≥2 nodes.

### Dependencies
- Phase 1 complete
- Node geo-cell ownership implemented

### Timeline Estimate
2-3 weeks

---

## Phase 3 — Association Detection

**Goal:** Turn raw co-occurrence into device-association graph.

### Deliverables
- [ ] Windowed join logic (co-location detection)
- [ ] `co_occurrence_events` table + batch job
- [ ] `association_edges` table + aggregation
- [ ] `association_strength` scoring prototype
- [ ] Synthetic data generator (known-paired vs known-incidental)

### Exit Criteria
Synthetic co-located device pairs correctly flagged; unrelated devices not flagged.

### Dependencies
- Phase 2 complete
- **BLOCKING:** Association strength formula validated against synthetic data

### Timeline Estimate
3-4 weeks (includes synthetic data generation and validation)

---

## Phase 4 — Signal Node / Aggregator / LoRa Tier

**Goal:** Bring in the low-cost coverage-extension hardware tier.

### Deliverables
- [ ] Signal node firmware (capture, rate-limit, sign, broadcast over LoRa)
- [ ] Aggregator logic (verify, enrich, compute deterministic occurrence_id)
- [ ] MQTT topic scheme for signal-node → aggregator → full node
- [ ] Multi-aggregator overlap dedup validation

### Exit Criteria
A signal-node ping heard by two aggregators results in exactly one stored occurrence.

### Dependencies
- Phase 3 complete
- SignalPing wire format finalized
- LoRa/Meshtastic hardware available

### Timeline Estimate
4-6 weeks (hardware-dependent)

---

## Phase 5 — Scale to N Full Nodes

**Goal:** Stress-test under realistic fleet size.

### Deliverables
- [ ] CA automation (step-ca or equivalent)
- [ ] Gossip protocol (SWIM) implementation
- [ ] H3 macro-cell ownership assignment
- [ ] Re-sharding logic (node ownership boundary changes)
- [ ] Load-test replication and query fan-out

### Exit Criteria
Node count scales to target fleet size (e.g., 250 Meshtastic nodes in NYC) with acceptable performance.

### Dependencies
- Phase 4 complete
- **BLOCKING:** Re-sharding logic designed and tested

### Timeline Estimate
6-8 weeks

---

## Phase 6 — Hardening

**Goal:** Security, observability, and legal compliance.

### Deliverables
- [ ] mTLS enforcement across all node-to-node traffic
- [ ] Access control on APIs
- [ ] Encryption at rest
- [ ] Observability (metrics/logging aggregation)
- [ ] Finalized retention/compaction policy
- [ ] Legal/privacy review

### Exit Criteria
System ready for production deployment (post-legal review).

### Dependencies
- Phase 5 complete

### Timeline Estimate
4-6 weeks (legal review timeline unknown)

---

## Cross-Cutting Research Items (Not Phase-Locked)

### Geo-Indexing (H3)
- [x] Pull/ingest current `h3-pg` documentation - See [`../technical/h3-pg-extension.md`](../technical/h3-pg-extension.md)
- [ ] Finalize H3 resolution levels (res 6 macro vs res 9 fine)
- [ ] Automate `LIST` sub-partition provisioning
- [ ] Re-sharding logic for ownership changes

**Blocked on:** Real density data from Phase 0

### Density & Scale Assumptions
- [ ] Truncated `device_hash_short` byte length (birthday-bound calculation)
- [ ] Validate 10-20s rate-limit threshold
- [ ] Target density: 50 devices/sq mile (upper bound)

**Blocked on:** Real capture volume from Phase 0

### Storage / Retention
- [ ] Raw-tier retention window (N days)
- [ ] `pg_partman` automation for partition creation/rollup/drop

**Blocked on:** Phase 0 volume measurements

### Identity Resolution
- [ ] Validate fingerprint-match heuristics
- [ ] Validate temporal-adjacency heuristics
- [ ] IRK resolution (likely out of scope)

**Blocked on:** Real device data from Phase 0+

### Association Detection
- [ ] `association_strength` composite formula
- [ ] Evaluate `ST_ClusterDBSCAN` vs. windowed joins

**Blocked on:** Synthetic data generator

### Networking / Coordination
- [ ] Gossip protocol subscription scaling (hub/relay pattern)
- [ ] Fallback for signal-node cluster losing all aggregators
- [ ] Bandwidth budgeting for constrained links

**Blocked on:** Node count growth (Phase 5)

### Security / Legal
- [ ] Formal legal review (device-only vs. person tracking)
- [ ] CA operational model at scale (step-ca vs. Vault PKI)
- [ ] Cert rotation cadence for signing keys

**Blocked on:** Phase 6 (but should engage legal early)

### Provenance / Signature Verification
- [x] Finalize exact `canonical_encode` byte layout/field order (**COMPLETE** - see `canonical-cbor-spec.md`)
- [ ] **Implement CA enrollment protocol** (**BLOCKING for Phase 0.5**)
- [ ] **Implement CA credential verification** (**BLOCKING for Phase 0.5**)
- [ ] **Implement offline verification workflow** (**BLOCKING for production**)
- [ ] **Implement revocation checking** (**BLOCKING for production**)
- [ ] Decide signing-key rotation cadence
- [ ] Decide whether aggregator enrichment should get its own signature layer

**Blocked on:** None for implementation, but CA infrastructure is a **critical security blocker**

---

## Critical Path

### Immediate (Weeks 1-2)
1. ~~Resolve provenance canonical encoding~~ (**DONE** - spec finalized, implemented)
2. Verify h3-pg extension against installed version
3. Fix repo model / schema alignment
4. **Configure CA root key** (select CA tooling, generate/store root key)

### Short-Term (Weeks 3-8)
5. Complete Phase 0 (single-node prototype)
6. **Complete Phase 0.5 (CA infrastructure)** - **CRITICAL SECURITY BLOCKER**
   - CA enrollment protocol
   - Credential verification
   - Revocation checking
   - Offline verification workflow
7. Begin Phase 1 (two-node sync)

### Medium-Term (Months 3-6)
6. Complete Phases 2-3 (federated query, association detection)
7. Validate density/scale assumptions

### Long-Term (Months 6-12)
8. Complete Phases 4-5 (signal nodes, scale to N)
9. Complete Phase 6 (hardening, legal review)

---

## Risk Assessment

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| H3 extension incompatibility | High | High | Verify early, have fallback plan |
| Repo model/schema misalignment | High | High | Fix before Phase 0 |
| ~~Provenance encoding undefined~~ | ~~High~~ | ~~Critical~~ | ~~RESOLVED~~ |
| **CA infrastructure missing** | **High** | **Critical** | **Implement Phase 0.5 before production** |
| Legal review blocks deployment | Medium | High | Engage legal early (Phase 5-6) |
| Gossip doesn't scale | Medium | Medium | Design hub/relay pattern early |
| Association detection inaccurate | Medium | Medium | Synthetic data validation |
| Signal node hardware unavailable | Low | Medium | Phase 4 may slip |

---

## References

- **Phase 0 details:** [`status.md`](status.md)
- **Research topics:** [`../open-questions/research-topics.md`](../open-questions/research-topics.md)
- **Architecture:** [`../architecture/overview.md`](../architecture/overview.md)
- **Blind spots:** [`../AGENTS.md#blind-spots`](../AGENTS.md#blind-spots)
