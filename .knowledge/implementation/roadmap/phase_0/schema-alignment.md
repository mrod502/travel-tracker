# Schema/Model Alignment (Phase 0)

**Status:** ⚠️ **ACTION REQUIRED**  
**Date:** 2026-08-15

---

## Executive Summary

The database schema and repo models are **mostly aligned**, but there's one **critical mismatch** that must be resolved before Phase 0 can write data:

| Component | Status | Issue |
|-----------|--------|-------|
| Database Schema | ✅ Aligned with architecture | No action needed |
| Repo Models | ⚠️ **NEEDS FIX** | `device_hash` type mismatch |
| Application Code | ⚠️ **NEEDS FIX** | Uses wrong hash format |

---

## Critical Issue: Device Hash Format

### The Mismatch

| Layer | Current Type | Expected Type |
|-------|--------------|---------------|
| **Database Schema** | `TEXT` (64-char hex string) | TEXT (hex string) ✅ |
| **Repo Model** | `Vec<u8>` (32 bytes) | ❌ **SHOULD BE TEXT** |
| **App Code** | 32-byte hash | ❌ **SHOULD BE HEX STRING** |

### Impact

1. **SQLX won't map correctly** - Type mismatch between model and schema
2. **Query failures** - INSERT/SELECT will fail at runtime
3. **Storage inefficiency** - Even if fixed, we need to decide canonical format

### Decision Required

We must choose one format and update BOTH schema and model:

#### Option A: TEXT (Hex String) - **RECOMMENDED for Phase 0**

```sql
-- Schema
device_hash TEXT NOT NULL  -- 64-char lowercase hex

-- Model
pub device_hash: String  -- "e3b0c44298fc1c149afbf4c8996fb924..."
```

**Pros:**
- Human-readable in SQL queries (debugging, ad-hoc analysis)
- Matches architecture spec (`data-model.md`)
- Can convert to/from bytes easily

**Cons:**
- 2x storage (64 bytes vs 32)
- Slightly larger indexes

**Migration effort:** Low - just change model type

#### Option B: BYTEA (Raw Bytes)

```sql
-- Schema
device_hash BYTEA NOT NULL  -- 32 raw bytes

-- Model
pub device_hash: Vec<u8>  -- 32-byte array
```

**Pros:**
- Compact storage (32 bytes)
- More efficient indexes

**Cons:**
- Not human-readable in SQL
- Requires schema migration
- Architecture spec says TEXT

**Migration effort:** Medium - change schema + all models

---

## Recommended Fix (Option A - TEXT)

### Step 1: Update Repo Model

**File:** `repo/src/models/occurrence.rs`

```rust
// Change from:
pub device_hash: Vec<u8>,

// To:
pub device_hash: String,  // 64-char lowercase hex string
```

### Step 2: Update Application Code

**File:** `app/src/app.rs` (current implementation)

```rust
// Current (WRONG):
let device_hash = {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&device_address);
    hasher.finalize().to_vec()  // Returns Vec<u8>
};

// Fixed (CORRECT):
let device_hash = {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&device_address);
    hex::encode(hasher.finalize())  // Returns hex string
};
```

### Step 3: Update Occurrence Repository

**File:** `repo/src/repositories/occurrence_repo.rs`

```rust
// Ensure INSERT query uses TEXT type
sqlx::query!(
    r#"
    INSERT INTO occurrences (
        ...,
        device_hash,
        ...
    ) VALUES (
        ...,
        $5,  -- TEXT parameter
        ...
    )
    "#,
    ...
    &occurrence.device_hash,  // &String - matches TEXT
    ...
)
```

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

## Action Plan

### Immediate (Before First Data Write)

1. **Fix `device_hash` type in repo model**
   - Change `Vec<u8>` to `String`
   - Update builder method signature
   - Update tests

2. **Fix app code to generate hex string**
   - Use `hex::encode()` when computing hash
   - Update all hash computation sites

3. **Verify SQLX compilation**
   - Run `cargo check` in `repo/` and `app/`
   - Fix any type mismatches

### Deferred (Can Wait Until Later)

4. **Consider BYTEA migration** (if storage becomes critical)
   - Create migration to convert TEXT → BYTEA
   - Update all models to use `Vec<u8>`
   - Benchmark storage difference

---

## Testing After Fix

### Unit Tests

```rust
#[test]
fn test_device_hash_format() {
    let mac = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    
    // Should produce 64-char lowercase hex string
    let device_hash = hex::encode(Sha256::digest(&mac));
    
    assert_eq!(device_hash.len(), 64);
    assert!(device_hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(device_hash, "71d7e8a9...");  // Known value test
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_insert_with_text_device_hash() {
    let pool = create_test_pool().await;
    
    let occurrence = Occurrence::builder()
        .device_hash(&"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string())
        .build();
    
    // Should not fail on type mismatch
    let result = OccurrenceRepository::create(pool, &occurrence).await;
    assert!(result.is_ok());
}
```

---

## Future Considerations

### BYTEA Migration Path (If Needed)

If storage becomes a concern at scale:

```sql
-- Migration: TEXT → BYTEA
ALTER TABLE occurrences 
    ALTER COLUMN device_hash TYPE BYTEA 
    USING DECODE(device_hash, 'hex');

-- Update model back to Vec<u8>
-- Run benchmark to verify storage savings
```

**Expected savings:** ~50% reduction in `device_hash` storage (64 → 32 bytes per row)  
**Trade-off:** Lose human-readability in SQL queries

---

## References

- **Architecture Spec:** [`../../architecture/data-model.md`](../../architecture/data-model.md)
- **Schema Divergence Analysis:** [`../../implementation/schema-divergence.md`](../../implementation/schema-divergence.md)
- **Repo Divergence Analysis:** [`../../implementation/repo-divergence.md`](../../implementation/repo-divergence.md)
- **Phase 0 Overview:** [`README.md`](./README.md)
