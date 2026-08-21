# Fast Agent Context — Bluetooth Tracking System

## System Overview

This is a **decentralized Bluetooth Low Energy (BLE) device tracking system** that deploys a network of nodes to scan advertisement traffic, associate observations with GPS location, and detect co-location patterns between devices.

**Key principle**: Privacy by design — the system tracks devices only, with NO person identity linkage anywhere in the schema.

## Node Tiers (Build Order)

1. **Full node** (Phase 0 — IN PROGRESS) — Owns geo-partition, answers federated queries, local Postgres/PostGIS
2. **Light node** (Phase 2) — Scans + forwards to full node
3. **Signal node** (Phase 3) — Cheap LoRa-based coverage extension, stationary/pre-registered
4. **Aggregator node** (Phase 3) — Bridges signal nodes to MQTT

## Core Technologies

| Technology | Purpose |
|------------|---------|
| **PostgreSQL + PostGIS** | Geo-partitioned storage with H3 indexing |
| **h3-pg extension** | H3 geospatial indexing (res 6 for ownership, res 9 for occurrences) |
| **Ed25519 + CBOR** | Cryptographic provenance signing (canonical encoding) |
| **mTLS (step-ca)** | Node identity and secure communication |
| **bt_mon** | Bluetooth scanning library (btleplug/bluer backends) |
| **MQTT / LoRa** | Network transport (MQTT for full/aggregator, LoRa for signal) |

## Critical Current Status (Phase 0)

### ✅ Completed
- bt_mon (BLE scanning) — functional
- Database schema — core tables exist
- Node identity generation — working
- Rate limiting — implemented
- CLI enhancements — config files, query mode, stats

### 🚨 Blockers (Must Resolve Before Phase 0 Exit)
1. **Repo model / schema misalignment** — Models in `repo/src/models/` don't match database schema (see `.knowledge/implementation/repo-divergence.md`)
2. **Provenance signing** — Not yet implemented (CBOR + Ed25519)
3. **H3 geo-indexing integration** — Schema exists, not integrated into storage flow

### 📊 Implementation Status Summary
| Component | Status |
|-----------|--------|
| bt_mon (BLE scanning) | ✅ Functional |
| Database schema | ⚠️ Partial (divergences need fixing) |
| Repository layer | ❌ Broken (models don't match schema) |
| App (scanning + storage) | ⚠️ Partial (basic flow works, missing provenance) |
| MQTT / networking | ❌ Not started |
| Provenance & signing | ❌ Not started |
| H3 geo-indexing | ⚠️ Partial (schema exists, not integrated) |

## Key Design Principles

- **Decentralized coordination** — gossip-based membership (SWIM), no central registry
- **Self-certifying node identity** — `node_id` = SHA-256(signing_public_key)
- **Independent provenance** — every occurrence signed by origin node, verifiable offline
- **Eventual consistency** — append-only records, conflict-free by construction
- **H3 geo-partitioning** — time RANGE + geography LIST partitions

## When to Invoke Skills

- **`/skill canonical-cbor`** — CBOR serialization, signing occurrence data, signature verification
- **`/skill h3-geospatial-indexing`** — H3 geospatial indexing, spatial queries, location storage
- **`/skill provenance-verification`** — Node identity, occurrence authenticity, signature verification

## Key File Paths (Quick Reference)

### Application Code
- `app/src/main.rs` — CLI entry point
- `app/src/app.rs` — FullNode orchestration
- `app/src/config.rs` — Configuration management
- `app/src/node/` — FullNode, identity, rate limiter
- `app/src/provenance/` — CBOR encoding, signing, verification
- `repo/src/models/occurrence.rs` — Database models (may be out of sync with schema)
- `repo/src/repositories/` — Repository layer

### Database
- `db/src/migrations/` — PostgreSQL migration scripts
- `.env.database` — Database connection credentials

### Knowledge Base
- `.knowledge/architecture/overview.md` — System design deep dive
- `.knowledge/architecture/data-model.md` — Database schema reference
- `.knowledge/architecture/provenance.md` — Security & signing design
- `.knowledge/implementation/status.md` — Component-by-component status
- `.knowledge/implementation/repo-divergence.md` — Model/schema mismatches

## Quick Commands

```bash
# Database migrations
cargo run --bin db -- migrate apply

# Run the app
cargo run --bin app -- monitor

# Query occurrences
cargo run --bin app -- query --last "1h"

# View stats
cargo run --bin app -- stats

# Run tests
cargo test

# Format and lint
cargo fmt && cargo clippy
```

## Subagent Model Selection

This project has **solar-powered fast agents** configured for offloading computational work:

| Agent Name | Use Case | Model |
|------------|----------|-------|
| `fast-explore` | Code exploration, searching, discovery | qwen3.6-27b (solar) |
| `fast-general` | General tasks, code modifications | qwen3.6-27b (solar) |
| `researcher-27b` | Research and information gathering | qwen3.6-27b (solar) |

**How to use:**
```
# Instead of generic agent tool, specify the named agent:
agent → subagent_type: "fast-explore" or "fast-general"
```

This allows you to distribute workload across solar-powered compute resources.

## Current Phase: Phase 0 — Single-Node Prototype

**Exit criteria**: Working single-node system that captures Bluetooth occurrences with provenance signatures and stores them in the database.

**Next actions**:
1. Fix repo model to align with database schema
2. Implement provenance signing (CBOR + Ed25519)
3. Integrate H3 geo-indexing into storage flow
4. Validate capture volume with real hardware

---

*For complete architecture details, see `.knowledge/AGENTS.md`*
*For Rust coding conventions, see `.qwen/RUST_GUIDE.md`*
