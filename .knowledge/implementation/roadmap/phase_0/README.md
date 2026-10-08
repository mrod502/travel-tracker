# Phase 0: Single-Node Prototype

**Status:** 🚧 In Progress  
**Goal:** Validate data model and local API before any networking complexity  
**Target Exit:** Can query "what did this node see in the last hour" and "where"

---

## Scope

Phase 0 implements a **standalone Full Node** that:
1. Monitors Bluetooth Low Energy (BLE) advertisements on its local adapter
2. Signs each observation with its node identity
3. Stores observations in its local PostgreSQL database
4. Can answer time-range + geo queries against its own data

**Explicitly Out of Scope:**
- Multi-node sync (Phase 1)
- Signal nodes / LoRa tier (Phase 4)
- Federated queries (Phase 2)
- Association detection (Phase 3)

---

## Deliverables

### Core Functionality

- [x] **bt_mon library** - BLE scanning via btleplug (Linux/BlueZ backend)
- [x] **Database schema** - Migrations created for `occurrences`, `nodes` tables
- [ ] **Full Node implementation** - Complete capture → sign → store flow
- [ ] **Clock discipline** - NTP sync on single node
- [ ] **Rate limiting** - Validate 10-20s threshold against real capture volume
- [ ] **Provenance signing** - Canonical CBOR encoding + Ed25519 signatures

### Verification & Testing

- [ ] **Volume measurement** - Real device density capture metrics
- [ ] **Determinism test** - Canonical encoding produces identical bytes
- [ ] **Signature verification test** - Can verify stored occurrences
- [ ] **Query validation** - Time-range + geo queries return expected results

---

## Architecture

### Component Overview

```
┌─────────────────────────────────────────────────────────────┐
│                     Full Node (Phase 0)                     │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐ │
│  │   bt_mon     │───▶│    app       │───▶│    repo      │ │
│  │   (BLE       │    │  (orchestration)  │  (database)  │ │
│  │   scanner)   │    │               │    │              │ │
│  └──────────────┘    └───────┬───────┘    └──────────────┘ │
│                               │                             │
│                               ▼                             │
│                     ┌─────────────────┐                     │
│                     │  provenance     │                     │
│                     │  (signing)      │                     │
│                     └────────┬────────┘                     │
│                              │                              │
│                              ▼                              │
│                     ┌─────────────────┐                     │
│                     │ PostgreSQL +    │                     │
│                     │ PostGIS + h3    │                     │
│                     └─────────────────┘                     │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Data Flow

1. **Discovery**: `bt_mon` detects BLE advertisement via btleplug
2. **Event**: `DeviceAdded` or `DeviceUpdated` event emitted
3. **Enrichment**: `app` adds node context (location, timestamps)
4. **Signing**: `provenance` module creates canonical CBOR + Ed25519 signature
5. **Storage**: `repo` inserts occurrence with `ON CONFLICT DO NOTHING` dedup
6. **Query**: Application can query by time range, geo cell, device hash

---

## Node Trait Architecture

### Core Traits

```rust
/// Core node functionality - implemented by all node types
pub trait Node: Send + Sync {
    /// Unique node identifier (SHA-256 hash of signing public key)
    fn node_id(&self) -> &[u8];

    /// Sign a payload with this node's private key
    fn sign(&self, payload: &[u8]) -> Result<Signature>;

    /// Verify this node's own signature (for debugging/testing)
    fn verify_self_signature(
        &self,
        payload: &[u8],
        signature: &Signature,
    ) -> Result<()>;
}

/// Bluetooth monitoring capability
pub trait BluetoothMonitor: Send + Sync {
    /// Start scanning for BLE advertisements
    async fn start_scan(&self) -> Result<()>;

    /// Stop scanning
    async fn stop_scan(&self) -> Result<()>;

    /// Get stream of device events
    async fn device_events(&self) -> Result<impl Stream<Item = DeviceEvent>>;

    /// Check if adapter is powered
    async fn is_powered(&self) -> Result<bool>;
}

/// Location provider capability
pub trait LocationProvider: Send + Sync {
    /// Get current location (if available)
    fn current_location(&self) -> Option<Location>;

    /// Get fixed location (for stationary nodes)
    fn fixed_location(&self) -> Option<Location>;
}

/// Full node - combines all capabilities
pub struct FullNode<N: Node, M: BluetoothMonitor, L: LocationProvider> {
    node: N,
    monitor: M,
    location_provider: L,
    database: OccurrenceRepository,
    rate_limiter: RateLimiter,
}
```

### Trait Relationships

```
Node (core identity + signing)
    ├── BluetoothMonitor (scanning capability)
    ├── LocationProvider (location data)
    └── FullNode (combines all + adds rate limiting, storage)
```

---

## Full Node Implementation Plan

### Module Structure

```
app/src/
├── main.rs                 # CLI entry point
├── app.rs                  # Application orchestration (current)
├── config.rs               # Configuration parsing
├── error.rs                # Error types
├── node/                   # NEW: Node implementation
│   ├── mod.rs              # Node trait exports
│   ├── full.rs             # FullNode struct
│   ├── identity.rs         # NodeIdentity (signing keys)
│   └── rate_limiter.rs     # In-memory rate limiting
├── provenance/             # NEW: Signing module
│   ├── mod.rs              # Public API
│   ├── payload.rs          # CanonicalPayload struct
│   ├── encode.rs           # CBOR serialization
│   ├── sign.rs             # Ed25519 signing
│   └── verify.rs           # Signature verification
└── clock/                  # NEW: Clock discipline
    ├── mod.rs
    └── ntp.rs              # NTP sync (optional for Phase 0)
```

### Key Components

#### 1. Node Identity (`app/src/node/identity.rs`)

```rust
pub struct NodeIdentity {
    private_key: SigningKey,      // Ed25519 private key (32 bytes)
    public_key: VerifyingKey,     // Ed25519 public key (32 bytes)
    node_id: Vec<u8>,             // SHA-256(public_key) - 32 bytes
}

impl NodeIdentity {
    /// Generate new random keypair
    pub fn generate() -> Self;

    /// Load from existing key material
    pub fn from_keypair(private: &[u8], public: &[u8]) -> Result<Self>;

    /// Load from file (persisted key)
    pub fn load_from_file(path: &Path) -> Result<Self>;

    /// Persist key to file
    pub fn save_to_file(&self, path: &Path) -> Result<()>;
}
```

**Key Management:**
- On first run: generate new Ed25519 keypair
- Persist keys to `$DATA_DIR/node_identity.json`
- Node ID is derived as `sha256(public_key)`
- Never expose private key outside signing module

#### 2. Rate Limiter (`app/src/node/rate_limiter.rs`)

```rust
pub struct RateLimiter {
    cache: DashMap<Vec<u8>, Instant>,  // device_hash → last_seen
    threshold_ms: u64,
}

impl RateLimiter {
    /// Check if device should be rate-limited
    /// Returns true if event should be DROPPED
    pub fn is_rate_limited(&self, device_hash: &[u8]) -> bool;

    /// Record observation (updates last_seen)
    pub fn record(&self, device_hash: &[u8]);

    /// Get current rate limit threshold
    pub fn threshold_ms(&self) -> u64;
}
```

**Configuration:**
- Default: 15 seconds (midpoint of 10-20s range)
- Configurable via environment variable `RATE_LIMIT_MS`
- In-memory cache (no persistence needed across restarts)

**Validation Required:**
- Measure actual device broadcast rates in target environment
- Adjust threshold if too aggressive (miss data) or too lenient (overwhelm DB)

#### 3. Full Node (`app/src/node/full.rs`)

```rust
pub struct FullNode {
    identity: NodeIdentity,
    monitor: BluetoothMonitor,
    location: Location,  // Fixed or from GPS
    database: OccurrenceRepository,
    rate_limiter: RateLimiter,
}

impl FullNode {
    /// Create new FullNode instance
    pub async fn new(config: &Config) -> Result<Self>;

    /// Start the main event loop
    pub async fn run(&mut self) -> Result<()>;

    /// Handle device discovered event
    async fn handle_device_discovered(&self, device: &BluetoothDevice) -> Result<()>;

    /// Create and store occurrence from device
    async fn store_occurrence(&self, device: &BluetoothDevice) -> Result<()>;
}
```

#### 4. Provenance Module (`app/src/provenance/`)

See detailed specification in [`canonical-payload-spec.md`](./canonical-payload-spec.md)

**Key Functions:**

```rust
// Encode canonical payload
pub fn encode_payload(payload: &CanonicalPayload) -> Result<Vec<u8>>;

// Sign encoded payload
pub fn sign_payload(private_key: &SigningKey, payload: &[u8]) -> Result<Signature>;

// Verify signature
pub fn verify_signature(
    public_key: &VerifyingKey,
    payload: &[u8],
    signature: &Signature,
) -> Result<()>;
```

---

## Clock Discipline Strategy

### Single-Node Assumption (Phase 0)

For Phase 0, clock discipline is **minimal**:
- Use system clock (NTP-synced by host OS)
- Record both `observed_at` (system time) and `observed_at_node_local` (adapter time if available)
- No cross-node clock sync needed (single node only)

### Implementation

```rust
// app/src/clock/mod.rs
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn now_local(&self) -> DateTime<Utc>;  // Node-local (may drift)
}

pub struct SystemClock;  // Uses chrono::Utc::now()

// For Phase 0: just use SystemClock
// Future phases: add NTP client for sync correction
```

### Validation Required

- Measure system clock drift over 24h period
- Verify NTP is active on target deployment OS
- Document acceptable drift tolerance for Phase 1+ sync protocol

---

## Canonical Signed Payload Format

**Status:** ✅ **SPECIFIED**  
**Reference:** [`canonical-payload-spec.md`](./canonical-payload-spec.md)

### Field Set (12 fields, exact order)

```
0.  schema_version      -- u16 (must be first)
1.  origin_node_id      -- bytes[32] (SHA-256 hash)
2.  device_hash         -- bytes[32] (SHA-256 hash)
3.  device_address      -- Optional bytes[6] (BLE MAC)
4.  observed_at_node_local -- text (ISO 8601 UTC)
5.  rssi                -- i16 (signed)
6.  tx_power            -- Optional i16
7.  adv_type            -- Optional u8 (BLE only)
8.  location            -- Optional array[2] (lat, lon) if from node
9.  signal_payload      -- Optional bytes (raw signal data)
10. advertised_name     -- Optional text
11. signal_type         -- u8 (0=bluetooth, 1=wifi, etc.)
```

**Encoding:** CBOR via `ciborium` crate (pure Rust, RFC 8949 compliant)  
**Signing:** Ed25519 via `ed25519-dalek` crate (pure Rust)

---

## Schema/Model Alignment Status

### Current State

**Database Schema:** ✅ Aligned with architecture docs  
**Repo Models:** ⚠️ **NEEDS ALIGNMENT** - See issues below

### Known Issues

1. **Location Type Mismatch**
   - Schema: `GEOGRAPHY(POINT, 4326)` (PostGIS)
   - Model: `Option<PostgisPoint>` ✅ **FIXED** in current code

2. **Device Hash Type**
   - Schema: `TEXT` (hex string, 64 chars)
   - Model: `Vec<u8>` (32 bytes) ❌ **MISMATCH**

3. **Manufacturer Data**
   - Schema: `JSONB` (flexible structure)
   - Model: Uses JSONB via `signal_payload` ✅ **FIXED** in unified model

4. **Provenance Fields**
   - Schema: `signed_payload BYTEA`, `signature BYTEA`
   - Model: ✅ **PRESENT** in Occurrence struct

### Required Fixes

**Device Hash Format Decision Needed:**

Option A (Schema): Store as TEXT hex string
- Pros: Human-readable in SQL, debuggable
- Cons: 2x storage (64 bytes vs 32)
- Model change: `device_hash: String`

Option B (Model): Store as BYTEA raw bytes
- Pros: Compact (32 bytes)
- Cons: Not human-readable in SQL
- Schema change: `device_hash BYTEA`

**Recommendation:** Option A (TEXT hex) for Phase 0 simplicity, can migrate to BYTEA later if storage becomes critical.

---

## Exit Criteria

Phase 0 is complete when:

1. ✅ **Functional:** Single node can scan, sign, and store BLE occurrences
2. ✅ **Queryable:** Can execute: "show all devices seen in last hour"
3. ✅ **Queryable:** Can execute: "show all devices in geo cell X"
4. ✅ **Provenance:** Stored occurrences have valid signatures
5. ✅ **Determinism:** Canonical encoding produces identical bytes on repeated runs
6. ✅ **Metrics:** Real capture volume measured (devices/hour, occurrences/hour)
7. ✅ **Rate Limit:** 10-20s threshold validated against real data

---

## Testing Strategy

### Unit Tests

- [ ] `test_canonical_encoding_deterministic` - 100 encodes = identical bytes
- [ ] `test_round_trip` - encode → decode equals original
- [ ] `test_signature_verification` - sign → verify = success
- [ ] `test_signature_fails_on_tampering` - modified payload = verification error
- [ ] `test_rate_limiter` - rapid events within threshold = dropped

### Integration Tests

- [ ] `test_full_capture_flow` - device discovered → occurrence in DB
- [ ] `test_provenance_verification` - stored occurrence can be verified
- [ ] `test_query_by_time` - time-range query returns expected results
- [ ] `test_query_by_geo` - geo-cell query returns expected results
- [ ] `test_deduplication` - duplicate insertions handled correctly

### Manual Validation

- [ ] Measure actual BLE device density in target environment
- [ ] Validate rate limit threshold (adjust if needed)
- [ ] Verify clock drift over 24h period
- [ ] Test with variety of BLE devices (phones, wearables, beacons)

---

## Dependencies to Add

```toml
# app/Cargo.toml
[dependencies]
# CBOR encoding (provenance)
ciborium = "0.2"

# Ed25519 signatures (provenance)
ed25519-dalek = "2.0"

# NTP sync (clock discipline - optional for Phase 0)
ntp-client = "0.1"  # Or similar

# Caching (rate limiter)
dashmap = "6.0"
```

---

## Risks & Mitigations

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Bluetooth adapter incompatibility | Medium | High | Test on target hardware early; document supported adapters |
| Rate limit too aggressive | Medium | Medium | Start with 15s, adjust based on measured density |
| Rate limit too lenient | Medium | Medium | Monitor DB write volume; adjust threshold |
| Clock drift > acceptable | Low | Medium | Verify NTP is active; measure drift |
| Canonical encoding non-deterministic | Low | Critical | Extensive testing; fuzz with random data |
| DB schema migration needed | Medium | Medium | Fix schema/model alignment before Phase 0 exit |

---

## Timeline

**Estimated Duration:** 1-2 weeks

### Week 1
- Days 1-2: Implement provenance module (CBOR + signing)
- Days 3-4: Implement FullNode refactoring (traits, rate limiter)
- Days 5: Clock discipline (NTP integration if needed)

### Week 2
- Days 1-2: Integration testing
- Days 3-4: Manual validation (real environment testing)
- Day 5: Documentation, metrics collection, Phase 0 exit review

---

## References

- **Architecture:** [`../../architecture/overview.md`](../../architecture/overview.md)
- **Data Model:** [`../../architecture/data-model.md`](../../architecture/data-model.md)
- **Provenance:** [`../../architecture/provenance.md`](../../architecture/provenance.md)
- **Canonical CBOR:** [`./canonical-payload-spec.md`](./canonical-payload-spec.md)
- **Storage:** [`../../architecture/storage.md`](../../architecture/storage.md)
- **Roadmap:** [`../roadmap.md`](../roadmap.md)
