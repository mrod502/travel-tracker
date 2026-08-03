# bt_iden - Bluetooth LE Identity Resolution

[![crates.io](https://img.shields.io/crates/v/bt_iden.svg)](https://crates.io/crates/bt_iden)
[![Documentation](https://docs.rs/bt_iden/badge.svg)](https://docs.rs/bt_iden)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A reusable Rust library for assigning **stable logical identities** to Bluetooth Low Energy (BLE) advertisement observations using probabilistic identity resolution.

## Overview

`bt_iden` implements a heuristic-based engine that groups BLE advertisement observations likely to belong to the same physical device across address rotations. It is designed to be:

- **Generic**: Works with observations from BlueZ, raw HCI sockets, btmon logs, pcap captures, or synthetic data
- **Configurable**: Tunable scoring weights and thresholds
- **Deterministic**: Same inputs produce same outputs
- **Well-tested**: Comprehensive unit tests, property tests, and benchmarks

## Design Philosophy

Bluetooth LE privacy features intentionally prevent reliable tracking of unpaired devices. This library does **not** attempt to defeat privacy mechanisms. Instead, it provides **best-effort inference** based on observable characteristics that may remain stable:

- Manufacturer-specific data patterns
- Service UUID advertisements
- Advertisement structure (AD field ordering)
- Signal strength continuity
- Advertisement timing patterns
- Device appearance and names

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
bt_iden = "0.1.0"
```

Basic usage:

```rust
use bt_iden::{IdentityResolver, HeuristicIdentityResolver};
use bt_iden::config::ResolverConfig;
use bt_iden::models::{AdvertisementObservation, BluetoothAddress, AddressType};
use std::time::Instant;

// Create resolver
let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());

// Create observation
let now = Instant::now();
let obs = AdvertisementObservation::new(
    now,
    BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
    AddressType::PrivateResolvable,
)
.with_rssi(-65)
.with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

// Assign identity
let identity = resolver.observe(obs);
println!("Assigned identity: {}", identity);
```

## Features

### Configurable Scoring System

The resolver uses weighted scoring with configurable thresholds:

```rust
use bt_iden::{ResolverConfig, ScoringWeights};
use std::time::Duration;

let config = ResolverConfig::builder()
    .merge_threshold(100.0)           // Score needed to merge
    .matching_window(Duration::from_secs(120))  // Active window
    .max_identity_age(Duration::from_secs(600)) // Expire after
    .debug_logging(true)
    .build();
```

### Default Weights

| Feature | Weight | Description |
|---------|--------|-------------|
| Manufacturer ID | 30 | Exact match only |
| Service UUIDs | 30 | Jaccard similarity |
| Appearance | 15 | Device category |
| Field Layout | 15 | AD type ordering |
| Payload Similarity | 20 | Byte comparison |
| Time Continuity | 20 | Recency bonus |
| RSSI | 10 | Signal continuity |
| Name | 25 | Local name |
| Connectable | 5 | Flag match |

### Thresholds

| Threshold | Value | Description |
|-----------|-------|-------------|
| Merge | ≥50 | Score to merge into existing identity |
| Possible | ≥30 | Potential match (internal use) |
| Reject | <30 | Unlikely to be same device |

### Address Rotation Handling

The resolver automatically handles address changes when sufficient evidence exists:

```rust
let obs1 = AdvertisementObservation::new(t1, addr_a, ...)
    .with_manufacturer_data(0x004C, data);

let obs2 = AdvertisementObservation::new(t2, addr_b, ...)  // Different address
    .with_manufacturer_data(0x004C, data);                  // Same manufacturer

// Both observations resolve to the same identity
assert_eq!(resolver.observe(obs1), resolver.observe(obs2));
```

## Testing

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run property tests
cargo test proptest

# Run benchmarks
cargo bench

# Check formatting
cargo fmt --check

# Lint
cargo clippy --workspace --all-targets -- -D warnings
```

## Architecture

```
bt_iden/
├── src/
│   ├── lib.rs           # Crate documentation and re-exports
│   ├── models.rs        # Data structures (Observation, Identity, etc.)
│   ├── config.rs        # Configuration and builder
│   └── resolver.rs      # Trait and implementation
├── tests/               # Integration tests
└── benches/             # Criterion benchmarks
```

## Limitations

- **No guarantees**: Resolution is probabilistic
- **No persistence**: Identities not saved across restarts
- **Not thread-safe**: Wrap in `Mutex` for concurrent access
- **Memory bounded**: Old identities expire based on configuration

## License

MIT License - see [LICENSE](../LICENSE) for details.
