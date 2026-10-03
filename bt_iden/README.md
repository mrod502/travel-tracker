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
use bt_iden::models::{AdvertisementObservation, AddressType, BluetoothAddress};
use bt_iden::time::ObservationTime;

// Create resolver
let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());

// Create observation
let obs = AdvertisementObservation::new(
    ObservationTime::now(),
    BluetoothAddress::new([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
    AddressType::PrivateResolvable,
)
.with_rssi(-65)
.with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03]);

// Assign identity. `resolve` rather than `observe`, because the reasoning travels with it:
// the score, the features that produced it, and how much of the model the observation was
// able to feed.
let resolution = resolver.resolve(obs);
println!(
    "Assigned identity {} (coverage {:.0}% of the model)",
    resolution.identity.id(),
    resolution.outcome.coverage() * 100.0,
);
```

`ObservationTime` is a wall-clock reading plus an optional monotonic one. A live capture has both
and measures elapsed time on the monotonic clock, which cannot jump when the system clock is
corrected; a row read back from a database has only the wall clock and says so (`is_live()` is
false) instead of pretending to a precision it does not have. That pair is what makes historical
rows replayable.

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
| Manufacturer ID | 40 | Exact match only |
| Service UUIDs | 30 | Jaccard similarity |
| Appearance | 15 | Device category |
| Field Layout | 15 | AD type ordering |
| Payload Similarity | 20 | Byte comparison |
| Time Continuity | 25 | Recency bonus |
| RSSI | 10 | Signal continuity |
| Name | 25 | Local name |
| Connectable | 5 | Flag match |

Those nine sum to 185 and form the *designed* model. A given observation rarely carries all of them,
and an uncarried feature is not treated as a disagreement: its weight leaves the denominator
(`FeatureSources::coverage` reports the ratio), and a feature inferred from something else — a
service list read off `service_data` keys, say — is scored at `derived_evidence_factor` (0.5) rather
than at face value. A merge that happened on 32 % of the model should look like that in the report.

### Thresholds

| Threshold | Value | Description |
|-----------|-------|-------------|
| Merge | ≥40 | Score to merge into existing identity |
| Possible | ≥25 | Potential match (internal use) |
| Reject | <25 | Unlikely to be same device |
| Evidence floor | 0.35 | `min_evidence_ratio`: the score must also rest on at least this much of the designed model |

An exact address match is not a weighted feature (`Feature::weight` returns `0.0`): it scores
`manufacturer_id + time_continuity` and is exempt from the evidence floor, because a repeated
address identifies the device instead of resembling another observation of it.

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
│   ├── time.rs          # ObservationTime: wall clock + optional monotonic pair
│   ├── evidence.rs      # Feature / Datum accounting, coverage, the evidence floor
│   ├── ad.rs            # AD structure parsing (layout, appearance, manufacturer, flags)
│   ├── config.rs        # Configuration and builder
│   └── resolver.rs      # Trait and implementation
├── tests/               # Integration tests
└── benches/             # Criterion benchmarks
```

## Limitations

- **No guarantees**: Resolution is probabilistic
- **No persistence *in this crate***: a `DeviceIdentity` counts inside one process. Persistence is the
  consumer's job and lives in `app/src/identity/`, which keys rows on a feature fingerprint rather than
  on these ids — so a reprocess lands on the same device while the thresholds here stay tight
- **A fleet is separated only by what it advertises**: a difference in manufacturer ID, appearance,
  service list or advertised name is Direct-quality and refuses the merge (`vetoed_feature`; names are
  truncation-tolerant, so a 30-byte truncated advertisement is not a rename, and an exact address match
  still outranks the veto). What remains is payload shape, which is shared by a whole product line — so
  hardware that shares its firmware default name *and* manufacturer ID and layout still merges. Observed
  and fixed on the dev database's mock beacons: the first pass merged five distinct beacons into one
  identity, the same window now resolves five. Refusals name the vetoing feature, which is what makes
  either outcome reviewable rather than silent
- **Not thread-safe**: Wrap in `Mutex` for concurrent access
- **Memory bounded**: Old identities expire based on configuration

## License

MIT License - see [LICENSE](../LICENSE) for details.
