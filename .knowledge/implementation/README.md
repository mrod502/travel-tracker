# Implementation Documentation

Documentation about the current implementation status, divergences from the architectural plan,
and the phased roadmap.

**Last updated:** September 2, 2026

## Documents

| Document | Description |
|----------|-------------|
| [`status.md`](status.md) | **Start here** — per-component status, verified against the working tree |
| [`../../GAP_ANALYSIS.md`](../../GAP_ANALYSIS.md) | Deep validation of each component against the fully implemented end state |
| [`schema-divergence.md`](schema-divergence.md) | Database schema: planned vs. actual, plus unenforced invariants |
| [`repo-divergence.md`](repo-divergence.md) | Rust model layer vs. schema — historic misalignment resolved, residual gaps listed |
| [`provenance-status.md`](provenance-status.md) | Provenance, CA, and revocation: what exists, what is wired |
| [`geo-h3-alignment.md`](geo-h3-alignment.md) | Geo/H3 specification: signed cells, ownership derivation, workstreams WS0–WS6 |
| [`canonical-encoding-summary.md`](canonical-encoding-summary.md) | Canonical CBOR encoding implementation summary |
| [`h3-pg-verification-summary.md`](h3-pg-verification-summary.md) | h3-pg signature verification against PGXN docs |
| [`roadmap.md`](roadmap.md) | Phased implementation plan (Phase 0–6) with deliverables and exit criteria |
| [`roadmap/phase_0/`](roadmap/phase_0/) | Phase 0 checklist, canonical payload spec, clock discipline, rate limiting, schema alignment |

## Quick Links

- **Architecture:** [`../architecture/`](../architecture/)
- **Research topics:** [`../open-questions/research-topics.md`](../open-questions/research-topics.md)
- **Blind spots:** [`../AGENTS.md`](../AGENTS.md)

## Current Phase

**Phase 0 — Single-Node Prototype** (in progress: code-complete, never validated live)

- ✅ `bt_mon` scanning library, three backends
- ✅ Unified `occurrences` schema; migrations authored but **never executed**
- ✅ Repo models aligned with the schema; `h3o` genuinely used in `repo::geo`
- ✅ Occurrences are signed on capture with canonical CBOR + Ed25519
- ✅ CA credential issuance and revocation structures, driven by the CLI
- ⚠️ No runtime verification: no node checks a CA credential, and the revocation checker is always empty
- ❌ No live capture, no rate-limit validation, no measured clock drift
- ❌ No MQTT, no sync, no gossip, no HTTP API

**Blockers:**
1. Create a partition for the current month — without it every insert fails
2. Run the migrations against PostgreSQL 18 + PostGIS + h3-pg and settle `h3index` wire behaviour (WS0)
3. Fix the two build/test defects so CI can go green (`bt_mon` mock example, racy clock assertion)
4. Wire one verification path end to end

See [`status.md`](status.md) for the detail and [`../../GAP_ANALYSIS.md`](../../GAP_ANALYSIS.md)
for the evidence.
