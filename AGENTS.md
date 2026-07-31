# Agent Instructions

**Setup Checklist**:
1. Copy the relevant language guide from `languages/` into this file
2. Delete the `languages/` directory to avoid context pollution
3. Delete this `SETUP.md` file after completion
4. Review `.qwen/settings.json` for model configuration

**Note**: This file should be populated with project-specific documentation after setup. During the initial project setup, copy the relevant language-specific guide from `languages/` and remove guides for languages not used in this project to avoid polluting the agent's context.

---

## Project Overview

This is a Rust application for a DB/HTTP server application for tracking travel data.

## Key Components and Architecture

### Core Directory Structure
- **src/** - Main source code
- **src/bin/** - Binary executables
- **src/lib.rs** - Library entry point
- **src/main.rs** - Binary entry point
- **src/modules/** - Feature modules
- **src/utils/** - Utility functions
- **src/types/** - Type definitions
- **tests/** - Integration tests
- **benches/** - Benchmark tests
- **examples/** - Example programs
- **migrations/** - Database migrations

### Rust Best Practices

1. **Ownership and Borrowing**
   - Understand ownership, borrowing, and lifetimes
   - Use `&T` for immutable references, `&mut T` for mutable
   - Prefer ownership transfer over cloning
   - Use `Cow<'a, T>` (Clone on Write) when appropriate

2. **Error Handling**
   - Use `Result<T, E>` for recoverable errors
   - Use `Option<T>` for nullable values
   - Propagate errors with `?` operator
   - Use `thiserror` for library errors, `anyhow` for applications

3. **Trait Design**
   - Define traits for shared behavior
   - Use trait objects (`&dyn Trait`) for dynamic dispatch
   - Use generics for static dispatch
   - Implement standard traits (`Debug`, `Display`, `Clone`, etc.)

4. **Module Organization**
   - Keep modules focused and small
   - Use `pub(crate)` for crate-internal visibility
   - Use `mod.rs` or direct file naming (`module.rs`)
   - Re-export public API at crate root

## Coding Conventions

### Naming
- Use `snake_case` for functions, variables, and modules
- Use `PascalCase` for structs, enums, and traits
- Use `SCREAMING_SNAKE_CASE` for constants
- Use `camelCase` for type parameters (`T`, `U`, `I`)

### Documentation
- Use `///` for item documentation
- Use `//!` for module/crate documentation
- Include examples in doc comments
- Run `cargo doc` to generate documentation
- Use `cargo doc --open` to view locally

### Code Style
- Run `cargo fmt` for consistent formatting
- Run `cargo clippy` for linting
- Follow Rust API guidelines
- Use meaningful type names

## Development Workflow

### Build Commands
```bash
# Build in development mode
cargo build

# Build in release mode
cargo build --release

# Run the application
cargo run

# Run in release mode
cargo run --release

# Run specific binary
cargo run --bin myapp

# Run tests
cargo test

# Run specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture

# Run integration tests only
cargo test --test '*'

# Check code without building
cargo check

# Format code
cargo fmt

# Lint code
cargo clippy

# Generate documentation
cargo doc --open

# Build documentation
cargo doc
```

### Dependency Management
```bash
# Add dependency
cargo add <package>

# Add dev dependency
cargo add <package> --dev

# Add build dependency
cargo add <package> --build

# Update dependencies
cargo update

# Remove dependency
cargo remove <package>

# Check for outdated packages
cargo outdated
```

### Workspace Setup
For multi-crate projects, create a workspace:

```toml
# Cargo.toml (workspace root)
[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
tokio = "1.35"
serde = { version = "1.0", features = ["derive"] }
```

## Recommended Tooling

### CLI Tools
- **cargo** - Package manager and build tool
- **rustfmt** - Code formatter
- **clippy** - Linter
- **cargo-edit** - `cargo add/remove` commands
- **cargo-watch** - Watch and rebuild on changes
- **cargo-nextest** - Faster test runner
- **cargo-llvm-cov** - Code coverage

### IDE Support
- **rust-analyzer** - Language server (essential)
- **rust-analyzer extension** for VS Code
- **IntelliJ Rust** plugin for JetBrains IDEs

### Testing
- **cargo-nextest** - Parallel test runner
- **mockall** - Mock generation
- **tempfile** - Temporary files for tests
- **assert_cmd** - Command testing
- **predicates** - Assertion predicates

### Web/Framework Options
- **Axum** - Ergonomic web framework (recommended)
- **Actix-web** - High-performance web framework
- **Rocket** - Developer-friendly web framework
- **Tonic** - gRPC implementation
- **Warp** - Functional web framework

### Async Runtime
- **tokio** - Multi-threaded async runtime (recommended)
- **async-std** - Stdlib-like async runtime
- **smol** - Lightweight async runtime

### Serialization
- **serde** - Serialization framework (essential)
- **serde_json** - JSON support
- **serde_yaml** - YAML support
- **toml** - TOML support

### Database
- **sqlx** - Async SQL with compile-time checks (recommended)
- **diesel** - ORM and query builder
- **sea-orm** - Async ORM
- **rusqlite** - SQLite bindings

### Error Handling
- **anyhow** - Application error handling
- **thiserror** - Library error definitions

### Logging
- **tracing** - Structured logging (recommended)
- **tracing-subscriber** - Tracing subscribers
- **env_logger** - Simple logging
- **log** - Logging facade

## Docker Configuration

### Dockerfile.dev Updates

```dockerfile
FROM rust:1.75-bookworm

ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=${USER_UID}

# Install system dependencies (may need libs for specific crates)
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        openssh-client \
        less \
        vim \
        zsh \
        pkg-config \
        libssl-dev \
    && npm install -g @qwen-code/qwen-code@latest \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Install Rust tools
RUN rustup component add rustfmt clippy rust-analyzer \
    && cargo install cargo-watch cargo-nextest cargo-llvm-cov cargo-edit

# Non-root user setup
RUN groupadd --gid ${USER_GID} ${USERNAME} \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/zsh ${USERNAME}

# Create workspace (target/ is project-local, no special dirs needed)
RUN mkdir -p /workspace \
    && chown -R ${USERNAME}:${USERNAME} /workspace

RUN git config --system --add safe.directory /workspace

ENV GIT_TERMINAL_PROMPT=0

# ... rest of template entrypoint and user setup
USER ${USERNAME}
WORKDIR /workspace
```

### compose.yaml Environment Variables

```yaml
environment:
  HOME: /home/dev
  # Rust uses project-local target/, minimal env vars needed
  # Optional: Redirect cargo registry if outside project
  # CARGO_HOME: /home/dev/.cargo
  # RUSTUP_HOME: /home/dev/.rustup
  RUST_LOG: debug  # Logging level
  RUST_BACKTRACE: 1  # Enable backtraces on panic
```

### compose.yaml Volumes

Rust uses project-local `target/`, so no special cache volumes needed:

```yaml
volumes:
  - llm-home:/home/dev
  # No Rust cache volumes needed - target/ is project-local
```

## .gitignore Updates

Ensure these Rust-specific patterns are in `.gitignore`:

```gitignore
# Build output (CRITICAL - don't commit!)
target/

# Generated documentation
doc/

# Benchmark results
criterion/

# IDE and editor files
.vscode/
.idea/
*.swp
*.swo

# Environment files
.env
.env.local

# Test output
*.tokio

# Coverage
*.profraw
*.profdata
coverage/

# Release binaries
/release/

# Native build files (when using maturin for Python bindings)
*.whl
```

## Project Structure Example

```
.
├── .cargo/
│   └── config.toml
├── benches/
│   └── my_benchmark.rs
├── crates/
│   ├── core/
│   ├── api/
│   └── cli/
├── examples/
│   └── basic_usage.rs
├── migrations/
│   └── 001_initial.sql
├── src/
│   ├── bin/
│   │   └── main.rs
│   ├── lib.rs
│   ├── main.rs
│   ├── api/
│   ├── db/
│   ├── models/
│   ├── services/
│   ├── types/
│   └── utils/
├── tests/
│   ├── integration_test.rs
│   └── common/
│       └── mod.rs
├── Cargo.toml
├── Cargo.lock
├── rustfmt.toml
├── clippy.toml
└── README.md
```

## Cargo.toml Configuration

### Basic Configuration
```toml
[package]
name = "myapp"
version = "0.1.0"
edition = "2021"
authors = ["Your Name <you@example.com>"]
description = "Brief description"
license = "MIT"
readme = "README.md"

[dependencies]
# Async runtime
tokio = { version = "1.35", features = ["full"] }

# Web framework
axum = "0.7"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Database
sqlx = { version = "0.7", features = ["runtime-tokio-rustls", "postgres"] }

# Error handling
anyhow = "1.0"
thiserror = "1.0"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# HTTP client
reqwest = { version = "0.11", features = ["json"] }

# Config
toml = "0.8"

[dev-dependencies]
tokio-test = "0.4"
mockall = "0.12"

[profile.release]
lto = true
codegen-units = 1
opt-level = 3

[profile.dev.package."*"]
opt-level = 3
```

## Cargo Configuration (.cargo/config.toml)

```toml
[build]
jobs = 4

# Add linker arguments for faster builds
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=lld"]

[env]
# Environment variables for all cargo commands
RUST_LOG = { value = "info" }

[net]
git-fetch-with-cli = true
```

## Rust Best Practices by Domain

### Web/API Development
```rust
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Clone)]
struct AppState {
    // Database pool, config, etc.
}

#[derive(Serialize)]
struct ApiResponse<T> {
    data: T,
    message: String,
}

async fn health_check() -> StatusCode {
    StatusCode::OK
}

#[axum::debug_handler]
async fn get_user(
    State(state): State<AppState>,
    axum::extract::Path(id): Path<u64>,
) -> Result<Json<ApiResponse<User>>, AppError> {
    let user = state.db.get_user(id).await?;
    Ok(Json(ApiResponse {
        data: user,
        message: "Success".to_string(),
    }))
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/users/:id", get(get_user))
        .with_state(state)
}
```

### Error Handling Pattern
```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("User not found: {0}")]
    UserNotFound(u64),

    #[error("Internal server error")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::UserNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".into()),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}
```

### Async Patterns
```rust
use tokio::task::JoinSet;

async fn process_batch(items: Vec<Item>) -> Result<Vec<Result>, AppError> {
    let mut set = JoinSet::new();

    for item in items {
        set.spawn(async move {
            process_item(item).await
        });
    }

    let mut results = Vec::new();
    while let Some(res) = set.join_next().await {
        results.push(res??);
    }

    Ok(results)
}

async fn process_item(item: Item) -> Result<Result, AppError> {
    // Async processing
    Ok(result)
}
```

## Performance Considerations

1. **Compile Time**
   - Use `cargo-sweep` to clean old artifacts
   - Use `sccache` or `ccache` for caching
   - Split into crates for parallel compilation
   - Use incremental compilation (enabled by default)

2. **Runtime Performance**
   - Use `cargo-flamegraph` for profiling
   - Use `criterion` for benchmarking
   - Profile with `perf` or `valgrind`
   - Enable LTO for release builds

3. **Memory Management**
   - Use `Arc<T>` for shared ownership across threads
   - Use `Rc<T>` for single-threaded shared ownership
   - Use `Box<T>` for heap allocation
   - Avoid unnecessary cloning with references

## Security Best Practices

1. **Dependency Security**
   - Run `cargo audit` regularly
   - Use `cargo-audit` in CI/CD
   - Pin dependencies to specific versions
   - Review dependency permissions

2. **Memory Safety**
   - Rust provides memory safety by default
   - Be careful with `unsafe` blocks
   - Use `#[deny(unsafe_code)]` in libraries
   - Audit any `unsafe` code carefully

3. **Input Validation**
   - Validate all user input
   - Use `validator` crate for struct validation
   - Sanitize file paths and database queries
   - Use parameterized queries (sqlx does this)
