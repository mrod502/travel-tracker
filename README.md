# Bluetooth Tracking Application

A decentralized, distributed system for tracking Bluetooth Low Energy (BLE) device movements across a network of scanning nodes.

## Overview

This system deploys a network of nodes that scan Bluetooth advertisement traffic, associate observations with GPS location, and detect co-location patterns between devices. The architecture supports multiple node tiers (Full, Light, Signal, Aggregator) with decentralized coordination and cryptographic provenance verification.

**Privacy by design**: The system tracks devices only, with no person identity linkage anywhere in the schema.

## Quick Links

- **Architecture Documentation**: [`.knowledge/README.md`](.knowledge/README.md)
- **Implementation Status**: [`.knowledge/implementation/status.md`](.knowledge/implementation/status.md)
- **Phase 0 Roadmap**: [`.knowledge/implementation/roadmap/phase_0/README.md`](.knowledge/implementation/roadmap/phase_0/README.md)
- **API/CLI Usage**: [`FULLNODE_INTEGRATION_HANDOFF.md`](FULLNODE_INTEGRATION_HANDOFF.md)
- **Example Configuration**: [`config.example.toml`](config.example.toml)

## Tech Stack

- **Language**: Rust (2021 edition)
- **Database**: PostgreSQL 18 with PostGIS + h3-pg extension
- **Web Framework**: Axum
- **Async Runtime**: Tokio
- **Bluetooth**: bt_mon (btleplug/bluer backends)
- **Networking**: MQTT (full/aggregator nodes), LoRa/Meshtastic (signal nodes)
- **Security**: Ed25519 signing, mTLS (step-ca)

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
# Database migrations
cargo run --bin db -- migrate apply

# Run the application
cargo run --bin app -- --help

# Run tests
cargo test

# Code quality
cargo fmt && cargo clippy
```

## Configuration

Configuration can be provided via:
1. **CLI arguments** (highest priority)
2. **Environment variables**
3. **TOML config file** (lowest priority)

See [`config.example.toml`](config.example.toml) for all available options.

### Key Configuration

```bash
# Database
export DATABASE_URL="postgres://btmon:btmon@localhost:5432/travel"

# Node identity (for testing)
export NODE_ID="00000000-0000-0000-0000-000000000000"

# Fixed location (for Docker environments without GPS)
export BT_FIXED_LOCATION="40.6892,-74.0445"
```

## CLI Commands

```bash
# Monitor mode - scan and store Bluetooth occurrences
cargo run --bin app -- monitor

# Query occurrences
cargo run --bin app -- query --last "1h" --signal-type bluetooth

# View database statistics
cargo run --bin app -- stats
```

## Architecture Overview

### Node Tiers

| Tier | Role | Status |
|------|------|--------|
| **Full node** | Owns geo-partition, answers federated queries | ✅ Phase 0 |
| **Light node** | Scans + forwards to full node | Planned (Phase 2) |
| **Signal node** | Cheap LoRa-based coverage extension | Planned (Phase 3) |
| **Aggregator node** | Bridges signal nodes to MQTT | Planned (Phase 3) |

### Core Components

- **bt_mon**: Bluetooth scanning library (btleplug/bluer backends)
- **db**: PostgreSQL migration crate with h3-pg extension support
- **app**: FullNode implementation with CLI
- **repo**: Repository layer for data access (work in progress)

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
