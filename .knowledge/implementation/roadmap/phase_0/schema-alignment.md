# Schema/Model Alignment (Phase 0)

**Status:** ✅ **Aligned** — `device_hash` is raw bytes in every layer
**Date:** 2026-08-15, resolved statement rewritten 2026-10-08

---

## Executive Summary

The database schema and the repo models agree:

| Component | Status | Note |
|-----------|--------|------|
| Database Schema | ✅ Aligned with architecture | No action needed |
| Repo Models | ✅ Aligned | `device_hash: Vec<u8}` against `BYTEA` |
| Application Code | ✅ Aligned | Hashes to 32 bytes and passes them through |

---

## Device Hash Representation

`device_hash` is the 32-byte SHA-256 digest of the device address, and it stays those
32 bytes everywhere:

```sql
-- Schema
device_hash BYTEA NOT NULL  -- 32 raw bytes
```

```rust
// Model
pub device_hash: Vec<u8>  // the same 32 bytes
```

Three reasons this is the shape and not a text spelling of the digest:

- The column would need an encode on every write and a decode on every read to hold
  what its type already holds, and every one of those is a place the two can disagree.
- Half the index size. `device_hash` is one of the five occurrence indexes, so this is
  not a rounding error.
- A digest stored as text cannot be joined to one stored as bytes. `node_id`,
  `origin_node_id` and `signing_public_key` are `BYTEA`; making `device_hash` anything
  else means the same kind of value lives in two types for no reason.

Printing it is a separate concern: a log line or a debug command may render it however
a person needs to read it, and that rendering is never what gets stored or signed.

---

## Other Alignment Status

### ✅ Already Aligned

| Field | Schema | Model | Status |
|-------|--------|-------|--------|
| `origin_node_id` | BYTEA | `Vec<u8>` | ✅ Correct |
| `signed_payload` | BYTEA | `Vec<u8>` | ✅ Correct |
| `signature` | BYTEA | `Vec<u8>` | ✅ Correct |
| `location` | GEOGRAPHY | `PostgisPoint` | ✅ Correct |
| `signal_payload` | JSONB | `serde_json::Value` | ✅ Correct |
| `rssi` | SMALLINT | `i16` | ✅ Correct |
| `adv_type` | ENUM | `AdvType` | ✅ Correct |
| `address_type` | ENUM | `BleAddressType` | ✅ Correct |
| `location_source` | ENUM | `LocationSource` | ✅ Correct |

### ⚠️ Minor Issues (Non-blocking)

| Field | Schema | Model | Issue | Priority |
|-------|--------|-------|-------|----------|
| `tx_power` | SMALLINT | `Option<i16>` | ✅ Aligned | None |
| `alt_m` | REAL | `Option<f32>` | ✅ Aligned | None |
| `accuracy_m` | REAL | `Option<f32>` | ✅ Aligned | None |
| `schema_version` | SMALLINT | `i16` | ✅ Aligned | None |

---

## Where the alignment is pinned

Every `#[sqlx::test]` in `repo` builds a throwaway database from the migrations and
inserts real rows through the models, so a column type and a field type that disagree
fail a test rather than a deployment — `repo/tests/wire_types.rs`,
`repo/tests/device_identity.rs` and `repo/tests/partition_healing.rs` all write
`device_hash` that way. [`wire_types.rs`](../../repo/tests/wire_types.rs) is where the
hand-written wire conversions are pinned in particular.

---

## References

- **Architecture Spec:** [`../../architecture/data-model.md`](../../architecture/data-model.md)
- **Schema Divergence Analysis:** [`../../implementation/schema-divergence.md`](../../implementation/schema-divergence.md)
- **Repo Divergence Analysis:** [`../../implementation/repo-divergence.md`](../../implementation/repo-divergence.md)
- **Phase 0 Overview:** [`README.md`](./README.md)
