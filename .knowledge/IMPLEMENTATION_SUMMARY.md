# Knowledge Base Creation — Session Summary

**Date:** August 5, 2026
**Task:** Build out Application knowledge base for distributed Bluetooth tracking system
**Status:** ✅ Complete — **superseded on several points; read the banner below**

> **Update 2026-09-02.** This file is a session log of what was documented in August and is
> kept as such. Several of its status judgements are no longer true of the code and are listed
> here so nobody acts on them:
>
> | Claimed in August | Actual as of 2026-09-02 |
> |---|---|
> | "Repository layer ❌ Broken — models don't match schema" | ✅ Reconciled onto the unified `Occurrence` model |
> | "Provenance & signing ❌ not started" | ✅ CBOR + Ed25519 signing implemented and integrated into capture |
> | "Canonical encoding not finalized" | ✅ Specified **and implemented** |
> | "CA enrollment 0%" | ✅ Library + `ca-init`/`ca-enroll`/`ca-verify`/`ca-revoke`/`ca-generate-rsl` CLI |
> | "h3-pg ✅ VERIFIED" | ⚠️ Verified against documentation only — never executed |
> | "Rate limiting / GPS not implemented" | ✅ Both implemented |
> | Web framework Axum (root README) | ❌ No HTTP layer exists in the dependency tree |
>
> Current authority for status is [`implementation/status.md`](implementation/status.md), with
> evidence in [`../GAP_ANALYSIS.md`](../GAP_ANALYSIS.md).

---

## What Was Documented

### 1. Architecture Documentation (5 documents)

| Document | Description |
|----------|-------------|
| [`architecture/overview.md`](architecture/overview.md) | System design, node tiers, coordination model, data flow summary |
| [`architecture/data-model.md`](architecture/data-model.md) | Database schema, occurrence records, device identity resolution, association graph |
| [`architecture/data-flow.md`](architecture/data-flow.md) | End-to-end packet flow from BLE to storage, MQTT/LoRa transport, sync protocol |
| [`architecture/storage.md`](architecture/storage.md) | Postgres/PostGIS partitioning, H3 geo-indexing, retention policy |
| [`architecture/provenance.md`](architecture/provenance.md) | Node identity, signing/verification, mTLS trust model |

### 2. Implementation Analysis (5 documents)

| Document | Description |
|----------|-------------|
| [`implementation/status.md`](implementation/status.md) | Component-by-component status vs. plan, gaps for Phase 0 |
| [`implementation/schema-divergence.md`](implementation/schema-divergence.md) | Database schema comparison: planned vs. actual (JSONB vs BYTEA, H3 issues) |
| [`implementation/repo-divergence.md`](implementation/repo-divergence.md) | Rust model layer analysis and critical type mismatches |
| [`implementation/roadmap.md`](implementation/roadmap.md) | Phased implementation plan (Phase 0–6) with deliverables and exit criteria |
| [`implementation/README.md`](implementation/README.md) | Index for implementation documentation |

### 3. Technical Documentation (1 document)

| Document | Description |
|----------|-------------|
| [`technical/h3o-crate.md`](technical/h3o-crate.md) | H3 geospatial crate documentation, core types, patterns, anti-patterns |

### 4. Open Questions (1 document)

| Document | Description |
|----------|-------------|
| [`open-questions/research-topics.md`](open-questions/research-topics.md) | Consolidated open questions, TODOs, and research areas organized by area |

### 5. Index & Navigation (3 documents)

| Document | Description |
|----------|-------------|
| [`AGENTS.md`](AGENTS.md) | Main entry point with comprehensive blind spots section and quick navigation |
| [`README.md`](README.md) | Knowledge base index and getting started guide |
| [`IMPLEMENTATION_SUMMARY.md`](IMPLEMENTATION_SUMMARY.md) | This document |

---

## Key Findings

### 1. Critical Blockers for Phase 0

One critical blocker remains to be resolved before Phase 0 can proceed:

1. **Repo Model / Schema Misalignment** - The repo crate models do not match the database schema:
   - `node_id` (single UUID) vs. `origin_node_id` + `reporting_node_id` (two TEXT fields)
   - Location: separate NUMERIC lat/lon (model) vs. PostGIS GEOGRAPHY (schema)
   - Missing: `signed_payload`, `signature`, `geo_cell_fine`, `geo_cell_macro` fields
   - Type mismatches: JSONB vs. typed arrays, ENUMs vs. strings

**✅ Resolved:**

- **Canonical Signed Payload Encoding** - CBOR (RFC 8949) format specified. See [`architecture/canonical-cbor-spec.md`](architecture/canonical-cbor-spec.md) for complete specification. Uses `ciborium` crate (pure Rust) with deterministic encoding rules.

- **h3-pg Extension API Verification** - Function signatures have been verified against PGXN documentation. The correct function name is `h3_latlng_to_cell()` (no underscore between "lat" and "lng"). The extension is `h3` (not `h3_postgis`). See [`technical/h3-pg-extension.md`](technical/h3-pg-extension.md) for complete reference.

### 2. Implementation Status Summary

| Component | Status | Notes |
|-----------|--------|-------|
| bt_mon (BLE scanning) | ✅ Functional | Core functionality works |
| Database schema | ⚠️ Partial | Core tables exist but divergences need fixing |
| Repository layer | ❌ Broken | Models don't match schema |
| App (scanning + storage) | ⚠️ Partial | Basic flow works, missing provenance |
| MQTT / networking | ❌ Not started | Phase 1+ work |
| Gossip protocol | ❌ Not started | Phase 5 work |
| Provenance & signing | ❌ Not started | Phase 0 prerequisite |
| H3 geo-indexing | ⚠️ Partial | Schema exists, not integrated |

### 3. Major Divergences from Plan

| Planned | Implemented | Impact |
|---------|-------------|--------|
| `node_id` = SHA-256(signing_public_key) | `node_id` = UUID string | Breaks self-certifying identity model |
| `service_uuids` = JSONB | `service_uuids` = `BYTEA[]` | Type mismatch - schema and repo don't agree |
| `manufacturer_data` = JSONB | Structured fields in repo | Loss of flexibility for complex manufacturer data |
| Location = PostGIS GEOGRAPHY | Location = NUMERIC lat/lon | Loses PostGIS spatial functions |
| `device_hash` = TEXT (hex) | `device_hash` = BYTEA (32 bytes) | Storage and comparison differences |

---

## Knowledge Graph Structure

```
.knowledge/
├── AGENTS.md                          # Root entry point with blind spots
├── README.md                          # Index and navigation
├── IMPLEMENTATION_SUMMARY.md          # This file
│
├── architecture/                      # System design
│   ├── overview.md                    # System at a glance
│   ├── data-model.md                  # Tables and relationships
│   ├── data-flow.md                   # End-to-end flow
│   ├── storage.md                     # Database strategy
│   └── provenance.md                  # Security & verification
│
├── implementation/                    # Current state
│   ├── README.md                      # Overview
│   ├── status.md                      # What's done/missing
│   ├── schema-divergence.md           # Schema analysis
│   ├── repo-divergence.md             # Model analysis
│   └── roadmap.md                     # Future phases
│
├── technical/                         # Deep dives
│   └── h3o-crate.md                   # H3 library reference
│
└── open-questions/                    # Unknowns
    └── research-topics.md             # Organized research items
```

---

## Next Steps (Recommended)

*Status of each step as of 2026-09-02 in brackets.*

### Immediate (Before Phase 0)

1. **Resolve canonical encoding** — ✅ done, specified and implemented
2. **Fix repo model** — ✅ done, aligned with the schema

### Short-Term (Phase 0)

4. **Implement provenance signing** — ✅ done, signed on capture
5. **Add rate limiting** — ✅ done, per-device with a configurable threshold
6. **Validate capture volume** — ❌ still open; requires a live capture

### Medium-Term (Phase 1-3)

7. **Implement MQTT sync** — ❌ not started (`rust-mqtt` declared, unused)
8. **Build federated queries** — ❌ not started
9. **Prototype association detection** — ❌ not started; `bt_iden` exists as an unconsumed library

**Not on the August list, and now blocking:** run the migrations against a real database, and wire
verification so that signatures are actually consumed. (The partition half of this is done as of
2026-09-14: a horizon is provisioned by `202609021200` and extended by `db up`, and
`202609141353` + `OccurrenceRepository::create` create a missing month at insert time.)

---

## Blind Spots Identified

12 blind spots documented, organized by blocking phase:

**Phase 0 (Critical):**
1. ~~Canonical signed payload encoding~~
2. ~~Repo model / schema misalignment~~ ✅ RESOLVED

**Phase 0 (Resolved):**
- ~~h3-pg extension API verification~~ ✅ RESOLVED

**Phase 3:**
3. Association strength composite formula
4. Identity resolution heuristics validation

**Phase 4:**
5. SignalPing truncated hash size
6. Aggregator enrichment trust boundary

**Phase 5:**
7. Gossip protocol scaling
8. Re-sharding logic
9. H3 resolution validation

**Phase 6:**
10. Legal review boundary
11. Signing-key rotation policy

**Unknown:**
12. Performance characteristics at scale

---

## Documents Created Today

Total: **14 documents** created/updated

| # | Document | Lines of Markdown |
|---|----------|-------------------|
| 1 | `.knowledge/AGENTS.md` | ~260 lines (updated) |
| 2 | `.knowledge/README.md` | ~150 lines |
| 3 | `.knowledge/IMPLEMENTATION_SUMMARY.md` | ~200 lines |
| 4 | `.knowledge/architecture/overview.md` | ~200 lines |
| 5 | `.knowledge/architecture/data-model.md` | ~250 lines |
| 6 | `.knowledge/architecture/data-flow.md` | ~200 lines |
| 7 | `.knowledge/architecture/storage.md` | ~200 lines |
| 8 | `.knowledge/architecture/provenance.md` | ~200 lines |
| 9 | `.knowledge/implementation/status.md` | ~350 lines |
| 10 | `.knowledge/implementation/schema-divergence.md` | ~300 lines |
| 11 | `.knowledge/implementation/repo-divergence.md` | ~300 lines |
| 12 | `.knowledge/implementation/roadmap.md` | ~250 lines |
| 13 | `.knowledge/implementation/README.md` | ~50 lines |
| 14 | `.knowledge/technical/h3o-crate.md` | ~350 lines |
| 15 | `.knowledge/open-questions/research-topics.md` | ~350 lines |

**Total:** ~3,410 lines of documentation

---

## Files Not Created (As Per Task Requirements)

- No diagrams created (Mermaid files were referenced in scratch/bundle but not found in the workspace - those directories are git-ignored)
- No code changes made (as per "NO CODE CHANGES" requirement)

---

## Quality Notes

All documentation follows these principles:

✅ **Incremental** - Docs can be updated as work progresses  
✅ **Referenced** - Cross-references between related topics  
✅ **Actionable** - Clear "Action Required" for each blind spot  
✅ **Structured** - Organized by topic (architecture, implementation, technical)  
✅ **Searchable** - Table of contents and index files  
✅ **GitHub-flavored Markdown** - With Mermaid support where needed  

---

## Session Complete

This knowledge base provides a comprehensive foundation for:
- New team members to understand the system
- Implementers to know what's done, what's missing, and what's broken
- Architects to see where design decisions diverged from implementation
- Anyone to identify blind spots and open questions

**Total time invested:** Single session  
**Documentation coverage:** Complete for Phase 0 blockers and architecture  
**Next action:** Resolve the three critical Phase 0 blockers before proceeding with implementation
