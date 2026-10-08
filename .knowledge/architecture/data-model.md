# Data Model

## Design Principle

**Raw observation data is immutable, append-only ground truth.**

Everything derived (device identity resolution, association scoring) lives in separate, fully reprocessable tables so scoring/resolution logic can improve later without migrating live state — just rerun the batch job over history.

**This system tracks devices only, never people.** No identity/person linkage exists anywhere in the schema.

---

## Core Tables

### `occurrences` — The Core Append-Only Record

**One row per node observing one wireless signal advertisement. Never updated after insert.**

**Supports:** Bluetooth, WiFi, NFC, Zigbee, LoRaWAN (extensible via `signal_type` ENUM)

#### Key Fields

| Field | Type | Description |
|-------|------|-------------|
| `occurrence_id` | UUID | UUIDv7 (time-sortable) for full-node origin, or deterministic hash for aggregator-relayed |
| `signal_type` | signal_type ENUM | **bluetooth**, wifi, nfc, zigbee, lorawan |
| `origin_node_id` | BYTEA | The node that **captured and signed** this observation (32-byte SHA-256 hash) |
| **Relay tracking moved to `occurrence_relays` table** | — | See below for separation of concerns |
| `observed_at` | TIMESTAMPTZ | Sync-corrected UTC timestamp |
| `observed_at_node_local` | TIMESTAMPTZ | Raw node-local timestamp (for drift auditing) |
| `device_address` | BYTEA | Raw MAC/address (6 bytes for BLE/WiFi, nullable) |
| `advertised_name` | TEXT | Device name (if present) |
| `device_hash` | BYTEA | Pseudonymous ID (32-byte SHA-256 hash) |
| `adv_type` | adv_type | BLE only (connectable_adv, etc.) — NULL for non-BLE |
| `rssi` | SMALLINT | Signal strength in dBm |
| `tx_power` | SMALLINT | TX power (if present) |
| `signal_payload` | JSONB | **Signal-specific data** (consolidated):
  - Bluetooth: `service_uuids`, `manufacturer_data`, `raw_payload_hex`, `address_type`, etc.
  - WiFi: `ssid`, `bssid`, `channel`, `capabilities`, etc.
  - Extensible for future signal types |
| `location` | GEOGRAPHY(POINT, 4326) | PostGIS point (lon, lat) |
| `alt_m` | REAL | Altitude in meters |
| `accuracy_m` | REAL | GPS accuracy in meters |
| `location_source` | location_source | node_fixed, node_gps, interpolated, aggregator_fixed |
| `geo_cell_fine` | H3INDEX | Resolution 9 (~0.1 km² cells) — generated from location |
| `geo_cell_macro` | H3INDEX | Resolution 6 (~36 km² cells) — generated from location, used for geo-cell ownership (application-level sharding) |
| `signed_payload` | BYTEA | Canonical byte sequence origin node actually signed |
| `signature` | BYTEA | Ed25519 signature over `signed_payload` |
| `schema_version` | SMALLINT | For forward compatibility |
| `ingested_at` | TIMESTAMPTZ | When THIS node wrote the row |

#### Primary Key
`(occurrence_id, observed_at)`

**Why include `observed_at`?** Postgres requires all partition-key columns in the PK for a partitioned table. Since we partition by RANGE on `observed_at`, it must be included in the primary key.

#### Deduplication Strategy
`INSERT ... ON CONFLICT (occurrence_id, observed_at) DO NOTHING`

Handles both:
- Multi-aggregator relay duplicates
- MQTT QoS 1 duplicate delivery
- Peer sync duplicates

#### Provenance Verification

Both `signed_payload` and `signature` stored **verbatim** — not reconstructed later from other columns. This avoids ambiguity about canonicalization rules drifting across schema versions.

**Signature does NOT cover:**
- Aggregator-added location (for `aggregator_fixed` rows)
- Sync-corrected `observed_at`
- `ingested_at`

These are attributable to the `reporting_node_id`, not the origin node.

---

### `occurrence_relays` — Relay Provenance Tracking (Phase 4+)

**Tracks which node(s) relayed an occurrence on behalf of the origin node.**

**Purpose:** Distinguish between:
1. **What was observed** (`occurrences` table - the core data)
2. **Who reported it on behalf of whom** (`occurrence_relays` table - relay metadata)

#### Key Fields

| Field | Type | Description |
|-------|------|-------------|
| `occurrence_id` | UUID | Reference to the occurrence being relayed |
| `observed_at` | TIMESTAMPTZ | Must match occurrence timestamp |
| `geo_cell_macro` | H3INDEX | Must match occurrence geo_cell_macro |
| `reporting_node_id` | BYTEA | The node that wrote this relay record (32-byte SHA-256 hash) |
| `ingested_at` | TIMESTAMPTZ | When this relay was recorded |

**Primary Key:** `(occurrence_id, observed_at, geo_cell_macro, reporting_node_id)`

**Why separate from `occurrences`:**
- Occurrences are **immutable ground truth** about what was detected
- Relay metadata is **provenance of data flow** through the network
- MVP (Phase 0-3) doesn't need relay tracking (single-hop aggregators only)
- Phase 4+ (signal nodes + multi-hop relays) requires this separation

**Use Cases:**
- Track aggregator behavior (which aggregators report which signal nodes)
- Detect relay anomalies (an aggregator reporting occurrences it shouldn't see)
- Multi-hop relay provenance (Phases 5+)

**MVP Status:** ⚠️ **Not required for Phase 0-3** - can be added later without schema migration

---

### `occurrence_rollups` — Retention/Aggregation Tier

**Populated by periodic job that summarizes and drops aged-out raw partitions.**

Keeps long-term storage bounded regardless of raw traffic volume.

**One row per `(node, device, time window)` with:**
- Count of occurrences
- Min/max/avg RSSI
- Representative geo cell

**Retention flow:**
1. Raw `occurrences` for N days
2. Roll up into `occurrence_rollups`
3. Drop raw partition

---

## Derived Tables (Identity & Association)

### `device_identities` — Stable Canonical Entity

**One row per physical device as best inferred.**

Carries:
- `fingerprint` (manufacturer ID, service UUID set hash, AD payload structure hash, name pattern)
- `confidence_score`
- Resolution method used

**Purpose:** Stable identity across rotating BLE addresses.

---

### `device_address_links` — Address Resolution Mapping

**Maps each observed rotating `device_hash` to a candidate `identity_id`, with validity window and resolution method used.**

**Resolution Heuristics (most to least reliable):**
1. **Fingerprint match** — manufacturer/service data structure often stable across address rotations
2. **Temporal adjacency** — old address stops appearing right as new address starts, at same node/geo cell, in tight time window
3. **Name match** — weak alone, useful combined with #2
4. **IRK resolution** — cryptographic resolution (likely out of scope unless devices are controlled/paired)

**Run as periodic batch job, not real-time** — resolution quality improves with more observed data per device.

---

### `co_occurrence_events` — Raw Co-location Detection

**Raw output of windowed join detecting two device identities observed together.**

- Same/nearby node
- Overlapping time window
- Canonical ordering (`identity_a < identity_b`) avoids storing reversed duplicates

---

### `association_edges` — Aggregated Relationships

**Queryable relationship between two device identities.**

Tracks:
- `co_occurrence_count`
- `distinct_geo_cells`
- `distinct_days`
- Composite `association_strength` score

**Scoring Principle:**
Raw co-occurrence count alone is a weak signal (e.g., two devices near a busy transit stop). **Geo/day diversity is a stronger signal** — repeated co-location across *different* locations and *different* days indicates a real association more reliably than one long single-location session.

**Open question:** Composite scoring formula needs prototyping against synthetic known-paired vs known-incidental device data.

---

## Node Registry & Sync

### `nodes` — Local Cache of Known Peers

**NOT authoritative — authority is the CA cert + gossip liveness. This is just a local view.**

| Field | Type | Description |
|-------|------|-------------|
| `node_id` | BYTEA | SHA-256(signing_public_key), self-certifying (32 bytes binary) |
| `node_type` | node_type | full, light, aggregator, signal |
| `mtls_cert_fingerprint` | TEXT | Transport identity (NULL for signal nodes) |
| `signing_public_key` | BYTEA | Ed25519 public key (32 bytes) — EVERY tier has one |
| `signing_key_algo` | TEXT | 'ed25519' |
| `ca_credential` | BYTEA | CA's signature over (node_id || signing_public_key) |
| `fixed_lat` | DOUBLE PRECISION | NULL if mobile |
| `fixed_lon` | DOUBLE PRECISION | NULL if mobile |
| `owns_geo_cells` | H3INDEX[] | Res-6 H3 cells this full node owns |
| `registered_at` | TIMESTAMPTZ | When enrolled with CA |
| `last_seen_at` | TIMESTAMPTZ | Last gossip activity |
| `status` | node_status | active, suspected, down, revoked |

**Key insight:** `signing_public_key` + `ca_credential` together let any party verify any occurrence's origin offline:
1. Check `ca_credential` against CA's known root key (done once per node, cacheable)
2. Verify occurrence signature against `signing_public_key`
3. No live CA call needed at verify-time

---

### `sync_cursors` — Replication Progress

**Per-peer, per-direction replication progress.**

Supports store-and-forward sync over intermittent links.

Tracks:
- Peer node ID
- Direction (inbound/outbound)
- Last synced occurrence ID / timestamp
- Sync completion status

---

## SignalPing — Wire Format (LoRa/Meshtastic)

**Not part of Postgres schema — this is the binary format signal nodes emit.**

```protobuf
struct SignalPing {
    device_hash_short: [u8; N],   // Truncated hash (10-12 bytes recommended for headroom)
    rssi: i8,
    timestamp_offset: u16,        // Seconds since signal node's last mesh sync beacon
    short_id: u16,                 // Compact wire alias for node_id (32-byte digest too large)
    sequence_num: u32,            // Used in deterministic occurrence_id derivation
    signature: [u8; 64],          // Ed25519 signature over the above fields
}
```

**Signal nodes carry:**
- No GPS (keeps hardware cost down)
- No precise clock (keeps hardware cost down)

**Location and authoritative timestamp are filled in by receiving aggregator** (which has known fixed, pre-registered location).

**Aggregator responsibilities:**
1. Verify signal node's signature
2. Resolve `short_id` → full `node_id` (holds full node registry)
3. Fill in location (aggregator's fixed position)
4. Fill in corrected timestamp
5. Compute deterministic `occurrence_id`
6. Publish full record to MQTT

---

## Rate Limiting

**Enforced at the node, before write** (not a DB constraint):

Each node keeps in-memory/local-cache map of `device_hash → last_seen_at`. Advertisements within configured threshold (target: 10-20s, tunable per-node based on local device density) are dropped or folded into rolling aggregate rather than written.

**Open question:** Validate 10-20s threshold against real capture volume in Phase 0. May need per-node configuration (dense vs sparse deployment areas).

---

## Open Questions / TODOs

- [ ] Finalize `association_strength` composite formula — needs prototyping against synthetic paired/unpaired device data
- [ ] Validate 10-20s rate-limit threshold against real capture volume in Phase 0
- [ ] Confirm truncated `device_hash_short` byte length against target device density (birthday-bound calc required)

---

## References

- **Architecture:** [`overview.md`](overview.md)
- **Storage:** [`storage.md`](storage.md)
- **Provenance:** [`provenance.md`](provenance.md)
- **Research Topics:** [`../open-questions/research-topics.md`](../open-questions/research-topics.md)
