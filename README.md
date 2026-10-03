# Bluetooth Tracking Application

A decentralized, distributed system for tracking Bluetooth Low Energy (BLE) device movements across a network of scanning nodes.

## Overview

This system deploys a network of nodes that scan Bluetooth advertisement traffic, associate observations with GPS location, and detect co-location patterns between devices. The architecture supports multiple node tiers (Full, Light, Signal, Aggregator) with decentralized coordination and cryptographic provenance verification.

**Privacy by design**: The system tracks devices only, with no person identity linkage anywhere in the schema.

## Quick Links

- **Architecture Documentation**: [`.knowledge/README.md`](.knowledge/README.md)
- **Implementation Status**: [`.knowledge/implementation/status.md`](.knowledge/implementation/status.md)
- **Gap Analysis** (what is real vs. what the end state requires): [`GAP_ANALYSIS.md`](GAP_ANALYSIS.md)
- **Phase 0 Roadmap**: [`.knowledge/implementation/roadmap/phase_0/README.md`](.knowledge/implementation/roadmap/phase_0/README.md)
- **Revocation Workflow**: [`REVOCATION_WORKFLOW.md`](REVOCATION_WORKFLOW.md)
- **Example Configuration**: [`config.example.toml`](config.example.toml)
- **CLI Usage**: `cargo run --bin app -- --help` (there is no separate handoff document)

## Tech Stack

- **Language**: Rust (2021 edition; `db` and `bt_iden` use edition 2024)
- **Database**: PostgreSQL 18 with PostGIS + h3-pg extension
- **Async Runtime**: Tokio
- **Data access**: sqlx, parameterized queries
- **Bluetooth**: `bt_mon` (btleplug by default; bluer and mock behind features)
- **Identity resolution**: `bt_iden` (standalone library; not yet wired into the pipeline)
- **Provenance**: Ed25519 signatures over deterministic CBOR (`ciborium`)
- **Trust**: `ca` library crate — CA root key file, credential issuance, revocation status lists
- **Operator interface**: the `app` CLI. **There is no HTTP/API server** — no axum, hyper, or
  actix anywhere in the dependency tree
- **Networking**: MQTT and LoRa/Meshtastic are **designed but not implemented**; `rust-mqtt` is a
  declared dependency of `app` with no references in its source

## Development Environment

### Prerequisites

- Docker Desktop or Podman
- Docker Compose v2.x
- Git with SSH keys configured

### Start Development Environment

```bash
# Start services (PostgreSQL, LLM agent container)
docker compose up -d

# Enter the LLM container
docker compose exec llm zsh

# Inside container: run Qwen with LSP support
QWEN_STREAM_IDLE_TIMEOUT_MS=7200000 qwen --experimental-lsp
```

### Build & Run

```bash
# Apply migrations (the db binary takes up/down/reset, not "migrate apply")
cargo run --bin db -- --host database --port 5432 --user "$DB_USER" --db "$DB_NAME" up

# Create a new migration
cargo run --bin db -- new-migration <name>

# Run the application
cargo run --bin app -- --help

# Run tests
cargo test

# Code quality
cargo fmt && cargo clippy
```

**Before the first run:** the `occurrences` table is range-partitioned by month and the
migrations only create partitions for July and August 2026. Until a partition covering the
current month exists, every insert fails. See
[`.knowledge/implementation/schema-divergence.md`](.knowledge/implementation/schema-divergence.md).

## Configuration

Precedence, highest first — as implemented in `app/src/config/layered.rs`:

1. **Command-line flags**
2. **TOML config file**
3. **Environment variables** (process env, then `.env`)
4. **Built-in defaults**

Note that the config file *outranks* the environment. See
[`config.example.toml`](config.example.toml) for all available options.

### Key Configuration

```bash
# Database (DATABASE_URL, or the PG* variables)
export DATABASE_URL="postgres://btmon:btmon@localhost:5432/travel"

# Fixed location, for Docker environments without GPS
export BT_LOCATION_FIXED="40.6892,-74.0445"    # BT_FIXED_LOCATION is also accepted

# Res-6 H3 cells this node claims to own (comma-separated)
export BT_OWNS_CELLS="862a1072fffffff"

# Rate limiting and adapter
export BT_RATE_LIMIT_MS=15000
export BT_ADAPTER_ID=<hci0>
export BT_USE_MOCK_BACKEND=true     # requires building with --features mock
```

A `NODE_ID` variable does not choose the node's identity: `node_id` is SHA-256 of the node's
Ed25519 signing public key, which lives in the keypair under the data directory, and nothing
outside that directory can set it. What `NODE_ID` / `[node].id` / `--node-id` does instead is
assert the identity the key has to back — 64 hex characters, checked at startup, which stops
and prints both ids when they disagree. Leave it unset to accept the key the machine has. A
UUID there is refused by name: that is what this setting asked for before.

## CLI Commands

```bash
# Monitor — scan and store signed Bluetooth occurrences
cargo run --bin app -- monitor

# Query occurrences
cargo run --bin app -- query --last "1h" --signal-type bluetooth

# Query by H3 macro cell
cargo run --bin app -- query --geo-cell <u64>

# Database statistics
cargo run --bin app -- stats

# CA operations — nested under `ca` (library CA: a root key file plus these subcommands)
cargo run --bin app -- ca ca-init
cargo run --bin app -- ca ca-enroll        # issues the credential and writes the nodes row
cargo run --bin app -- ca ca-verify --node-id <hex>
cargo run --bin app -- ca ca-revoke
cargo run --bin app -- ca ca-info
cargo run --bin app -- ca ca-generate-rsl
```

A node refuses to scan until it has a row in `nodes`; run `ca ca-enroll` against that node's
database first.

`monitor --node-id` asserts the identity the node's key has to match; it cannot supply one,
since identity is the Ed25519 keypair in the data directory. The same value is accepted as
`--node-id` before the subcommand, as `[node].id`, or as `NODE_ID`.

The adapter and the scan cadence reach the radio the same way: `BT_ADAPTER_ID` names which
adapter the monitor opens (startup fails if it matches none, or more than one), and
`BT_SCAN_INTERVAL_MS` is how often that continuous scan is re-armed — and how often the mock
radio advertises. `BT_STORE_RAW_PAYLOAD` decides whether each occurrence keeps the radio's own
advertisement bytes in `signal_payload.ble.raw_payload_hex`, inside the signature.

## Architecture Overview

### Node Tiers

| Tier | Role | Status |
|------|------|--------|
| **Full node** | Owns geo cells, stores and queries occurrences | Phase 0 — implemented in code, never run live |
| **Light node** | Scans + forwards to a full node | Planned (Phase 2) |
| **Signal node** | Cheap LoRa-based coverage extension | Planned (Phase 4) |
| **Aggregator node** | Bridges signal nodes to MQTT | Planned (Phase 4) |

### Core Components

| Crate | Purpose | State |
|-------|---------|-------|
| `app` | FullNode binary: scan → rate-limit → sign → store, plus the CA CLI | Implemented; verification and revocation not wired into the runtime |
| `bt_mon` | BLE scanning library (btleplug / bluer / mock) | Working; raw advertisement payload not yet exposed |
| `repo` | sqlx models, repositories, and the client-side H3 module | Aligned with the schema |
| `db` | Migration tool (`new-migration`, `up`, `down`, `reset`) | Seven migrations; none ever executed |
| `ca` | CA root, credentials, revocation status lists, policies | Library complete; nothing consults it at runtime |
| `bt_iden` | Probabilistic device identity resolution | Standalone library; no consumer yet |

## Development Workflow

### Database

```bash
# Connect to database
docker compose exec postgres psql -U btmon -d travel

# Run migrations manually
cargo run --bin db -- --host database --port 5432 --user ${DB_USER} --db ${DB_NAME} up
```

### Testing

```bash
# All tests
cargo test

# Integration tests only
cargo test --test '*'

# With output
cargo test -- --nocapture
```

## Security Notes

- Never commit `.env` file with sensitive data
- Dependencies are pinned in `Cargo.lock`
- Run `cargo audit` regularly
- Use parameterized queries (sqlx does this by default)

## License

MIT
