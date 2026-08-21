# Rust Development Guide

## Project Overview

This is a Rust application for a DB/HTTP server application for tracking travel data.

## Key Components and Architecture

### Core Directory Structure

| Path | Purpose |
|------|---------|
| `db/` | Database migration crate - PostgreSQL schema setup and migration management via CLI |

## IMPORTANT RULES

- **NEVER** manually modify tables. **ALWAYS** use the migration utility.

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
   - Pin dependencies to specific versions
   - Review dependency permissions

2. **Memory Safety**
   - Rust provides memory safety by default
   - Be careful with `unsafe` blocks
   - Use `#[deny(unsafe_code)]` in libraries
   - Audit any `unsafe` code carefully

3. **Input Validation**
   - Validate all user input
   - Use `validator` crate for validation
   - Sanitize file paths and database queries
   - Use parameterized queries (sqlx does this)
