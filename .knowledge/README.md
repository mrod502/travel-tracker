# Bluetooth Tracking Application — Knowledge Base Index

This directory contains the complete knowledge base for the distributed Bluetooth Low Energy (BLE) device tracking system.

## Directory Structure

```
.knowledge/
├── AGENTS.md                    # Main entry point with blind spots & quick navigation
├── README.md                    # This file - index and overview
│   (../GAP_ANALYSIS.md          # Repo root: per-component validation vs the end state)
│
├── architecture/                # System design & architecture
│   ├── overview.md             # High-level system design, node tiers
│   ├── data-model.md           # Database schema, records, identity resolution
│   ├── data-flow.md            # End-to-end packet flow, MQTT/LoRa transport
│   ├── storage.md              # Postgres/PostGIS, H3 partitioning, retention
│   ├── provenance.md           # Node identity, signing/verification, mTLS
│   ├── canonical-cbor-spec.md  # Canonical CBOR encoding for signed payloads
│   ├── ca-infrastructure-design.md      # CA as a library crate: options and decision
│   ├── mqtt-v5.md              # MQTT v5 protocol reference (reference material)
│   ├── tarantool-swim.md       # Gossip/SWIM design notes (design, not implementation)
│   └── specifications/
│       ├── revocation-scheme.md         # RSL-based node revocation design
│       └── rfc_7250/{adaptation.md,main.txt}
│
├── implementation/              # Current implementation status
│   ├── README.md               # Implementation docs overview
│   ├── status.md               # Per-component status, verified 2026-09-02
│   ├── schema-divergence.md    # Database: planned vs. actual, unenforced invariants
│   ├── repo-divergence.md      # Rust models vs. schema (misalignment resolved)
│   ├── provenance-status.md    # Provenance / CA / revocation: built vs. wired
│   ├── geo-h3-alignment.md     # Signed geo cells, ownership derivation, WS0-WS6
│   ├── canonical-encoding-summary.md
│   ├── h3-pg-verification-summary.md
│   ├── roadmap.md              # Phased plan (Phase 0-6)
│   └── roadmap/phase_0/        # Phase 0 checklist + payload, clock, rate-limit, schema specs
│
├── technical/                   # Technical deep dives
│   ├── h3o-crate.md            # H3 geospatial crate documentation
│   └── h3-pg-extension.md      # h3-pg Postgres extension reference
│
└── open-questions/              # Research topics & TODOs
    └── research-topics.md      # All open questions organized by area
```

## Getting Started

### New to the Project?
Start with: [`architecture/overview.md`](architecture/overview.md) → [`AGENTS.md`](AGENTS.md)

### Ready to Implement?
Start with: [`implementation/status.md`](implementation/status.md) → [`implementation/roadmap.md`](implementation/roadmap.md)

### Database Work?
Start with: [`architecture/storage.md`](architecture/storage.md) → [`implementation/schema-divergence.md`](implementation/schema-divergence.md)

### Rust Development?
Start with: [`technical/h3o-crate.md`](technical/h3o-crate.md) → [`implementation/repo-divergence.md`](implementation/repo-divergence.md)

---

## System Overview

A decentralized network of nodes scanning Bluetooth advertisement traffic, associating observations with GPS location, and detecting co-location patterns between devices.

### Node Tiers
- **Full node:** Owns geo-partition, answers queries, runs aggregation
- **Light node:** Scans + forwards (future)
- **Signal node:** Cheap LoRa-based coverage extension
- **Aggregator node:** Bridges signal nodes to MQTT

### Core Technologies
- **Database:** Postgres + PostGIS + h3-pg extension
- **Geo-indexing:** H3 hexagonal grid (res 6 for ownership, res 9 for indexing)
- **Bluetooth:** bt_mon library (btleplug/bluer backends)
- **Networking:** MQTT (full/aggregator), LoRa/Meshtastic (signal)
- **Security:** Ed25519 signing, mTLS (step-ca), self-certifying node IDs

---

## Current Phase

**Phase 0 — Single-Node Prototype** (in progress: code-complete, never validated live)

The scan → sign → store path exists in code. What blocks Phase 0 is validation and one missing
piece of infrastructure, not missing features:

1. 🚨 **Migrations have never been executed** (no Postgres/PostGIS/h3-pg in the dev container)
2. 🚨 **`h3index` wire behaviour unconfirmed** (WS0) — gates the geo workstreams
3. ⚠️ **Provenance is write-only** — signatures are produced, nothing verifies them; the runtime revocation checker is always empty

Resolved since the August snapshot: canonical payload encoding (CBOR, implemented), repo
model/schema alignment (unified `Occurrence`), CA credential issuance (library + CLI), and the
partition gap — `202609021200` opens a 16-month horizon that `db up` re-extends on every deploy,
and `202609141353` + `OccurrenceRepository::create` create the month a row needs when a write finds
it missing.

See [`implementation/status.md`](implementation/status.md) for progress and
[`../GAP_ANALYSIS.md`](../GAP_ANALYSIS.md) for the evidence behind each claim.

---

## Quick Reference Table

| Topic | Primary Doc | Related Docs |
|-------|-------------|--------------|
| System architecture | [`architecture/overview.md`](architecture/overview.md) | All architecture/* |
| Database schema | [`architecture/data-model.md`](architecture/data-model.md) | storage.md, schema-divergence.md |
| Provenance/security | [`architecture/provenance.md`](architecture/provenance.md) | overview.md, research-topics.md |
| Data flow | [`architecture/data-flow.md`](architecture/data-flow.md) | data-model.md, storage.md |
| Current status | [`implementation/status.md`](implementation/status.md) | roadmap.md, repo-divergence.md |
| H3 geo-indexing | [`technical/h3o-crate.md`](technical/h3o-crate.md) | storage.md, research-topics.md |
| Open questions | [`open-questions/research-topics.md`](open-questions/research-topics.md) | AGENTS.md (blind spots) |
| **Blind spots** | **[`AGENTS.md#blind-spots`](AGENTS.md#blind-spots)** | research-topics.md |

---

## Document Maintenance

This knowledge base is a living document. When contributing:

1. **Update incrementally** — Don't let docs go stale; update as you discover/fix things
2. **Reference properly** — Link to related docs instead of duplicating content
3. **Close loops** — When you resolve a blind spot or open question, update all affected docs
4. **Flag assumptions** — If something is unvalidated, mark it as a blind spot
5. **Keep indices current** — Update AGENTS.md table of contents when adding new docs

---

## External References

- **H3 geo-indexing:** https://h3geo.org/docs/
- **h3o Rust crate:** `cargo doc --package h3o --open`
- **PostGIS:** https://postgis.net/documentation/
- **step-ca (PKI):** https://smallstep.com/docs/step-ca/
- **MQTT:** https://mqtt.org/
- **LoRa/Meshtastic:** https://meshtastic.org/

---

## Contributing

To add new documentation:
1. Create file in appropriate directory (architecture/, implementation/, technical/, open-questions/)
2. Add entry to AGENTS.md table of contents
3. Add entry to this README index

To update existing docs:
1. Make changes
2. Update "Last updated" timestamp if applicable
3. Check all docs that reference this one for consistency
