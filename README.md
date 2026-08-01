# Travel Tracker

A Rust-based DB/HTTP server application for tracking travel data.


## Tech Stack

- **Language**: Rust (2021 edition)
- **Web Framework**: Axum (recommended)
- **Database**: PostgreSQL with sqlx
- **Async Runtime**: Tokio
- **Serialization**: Serde + serde_json
- **Error Handling**: anyhow + thiserror
- **Logging**: tracing + tracing-subscriber

## Directory Structure

```
.
├── .cargo/
│   └── config.toml
├── migrations/
│   └── 001_initial.sql
├── src/
│   ├── bin/
│   │   └── main.rs
│   ├── lib.rs
│   ├── api/          # HTTP API handlers
│   ├── db/           # Database operations
│   ├── models/       # Data models
│   ├── services/     # Business logic
│   ├── types/        # Type definitions
│   └── utils/        # Utility functions
├── tests/
│   └── integration_test.rs
├── Cargo.toml
├── Cargo.lock
└── README.md
```

## Prerequisites

- Docker Desktop or Podman installed
- Docker Compose v2.x
- Git configured with your identity
- SSH keys set up for private repository access (if applicable)

## Quick Start

### 1. Start the LLM development environment

```bash
docker compose up -d llm
docker compose exec llm zsh
QWEN_STREAM_IDLE_TIMEOUT_MS=7200000 qwen --experimental-lsp
```

### 2. Initialize Rust project (first time only)

Inside the container:

```bash
cargo init
```

### 3. Add dependencies

```bash
cargo add tokio --features full
cargo add axum
cargo add serde --features derive
cargo add serde_json
cargo add sqlx --features runtime-tokio-rustls,postgres
cargo add anyhow thiserror
cargo add tracing tracing-subscriber --features env-filter
```

## Development Workflow

### Build & Run

```bash
# Build in development mode
cargo build

# Build in release mode
cargo build --release

# Run the application
cargo run

# Run in release mode
cargo run --release
```

### Testing

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture
```

### Code Quality

```bash
# Format code
cargo fmt

# Lint code
cargo clippy
```

## Docker Configuration

### Services

- **llm**: LLM coding agent container with Rust toolchain
- **database**: PostgreSQL 18 database
- **server**: Application server (when built)

### Volumes

- `llm-home`: Persists LLM agent state and settings
- `db-data`: Persists PostgreSQL data

### Environment Variables

See `.env` file for configuration:
- `DEV_UID/DEV_GID`: Match host user for file permissions
- `DB_PASSWORD`: Database password
- `DB_PUBLIC_PORT`: PostgreSQL port (default: 5432)
- `LISTEN_PORT`: Application port (default: 8080)

## Project Setup Checklist

- [ ] Initialize Cargo project with `cargo init`
- [ ] Configure `Cargo.toml` with project metadata
- [ ] Add required dependencies
- [ ] Set up database migrations in `migrations/`
- [ ] Configure `.cargo/config.toml` for build optimizations
- [ ] Create `rustfmt.toml` for code formatting
- [ ] Implement core application logic
- [ ] Write tests in `tests/`

## Security Notes

- Never commit `.env` file with sensitive data
- Dependencies are pinned in `Cargo.lock`
- Run `cargo audit` regularly to check for vulnerabilities
- Use parameterized queries (sqlx does this by default)

## License

MIT
