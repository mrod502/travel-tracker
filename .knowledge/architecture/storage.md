# Storage Strategy

**Engine:** Postgres + PostGIS

**Why:** Familiarity and flexibility over purpose-built time-series/geo database.

## Consistency Model

**Eventual consistency** is acceptable:
- Append-mostly sensor/observation data
- No contended multi-writer updates
- Conflict-free by construction (see [data-flow](data-flow.md))
- Every `occurrences` row written exactly once, never mutated afterward
- Conflicts avoided rather than resolved

---

## Partitioning Strategy

### Two-Level Physical Partitioning (within each full node's Postgres)

```sql
-- Level 1: RANGE on time (monthly)
-- Level 2: LIST on H3 macro cell (geography)
```

#### Level 1: Time Partitioning (RANGE on `observed_at`)

**Monthly partitions for retention/compaction:**
- Old partitions get summarized into `occurrence_rollups` and dropped
- Example:
  ```sql
  CREATE TABLE occurrences_2026_07 PARTITION OF occurrences
      FOR VALUES FROM ('2026-07-01') TO ('2026-08-01');
  CREATE TABLE occurrences_2026_08 PARTITION OF occurrences
      FOR VALUES FROM ('2026-08-01') TO ('2026-09-01');
  ```

**Creation, as implemented:** `ensure_occurrence_partitions(months_ahead)` opens a horizon of
monthly partitions and `db up` calls it after every deploy. `ensure_occurrence_partition(ts)`
creates the single month holding one timestamp, and `OccurrenceRepository::create` calls it when an
insert is refused for want of a partition, then retries — so a missed schedule costs a slow first
write rather than the write path. There is deliberately **no DEFAULT partition**: it would hide a
missed horizon inside rows filed off the monthly scheme.

**Still manual:** rollup into `occurrence_rollups` and the drop of aged partitions. Nothing in the
schema deletes data; retention is a separate, explicit operation.

#### Level 2: Geo Partitioning (LIST on `geo_cell_macro`)

**Why LIST on H3 cells (not raw lat/lon):**
- H3 macro cells are a **discrete, enumerable set**
- Unlike raw lat/lon, which has no native 2D partitioning support in Postgres
- H3 discretizes geography into something Postgres partitioning machinery handles directly

**Why H3 over geohash or flat PostGIS grid:**
1. **Uniform neighbor distance** — hexagons vs. geohash's rectangular cells with uneven edge/corner neighbor distances
   - Matters for "expand search to nearby cells" logic in association detection and node coverage queries
2. **Hierarchical resolution levels** — ownership (coarse, e.g., res 6) and per-occurrence indexing (fine, e.g., res 9) share one indexing scheme
   - Converted via `h3_cell_to_parent` rather than recomputed from raw lat/lon
3. **No pole/antimeridian distortion** — unlikely to matter for NYC specifically, but relevant if generalizing toward NYCMesh-integration stretch goal

**Node owns sub-partitions only for cells it owns:**
- `owns_geo_cells` (H3INDEX[]) determines which macro-cell sub-partitions each full node provisions
- Queries prune on geography natively, not just via index

**Operational consequence:**
- Adding a new owned macro cell = provisioning new LIST sub-partition
- Automatable, but not automatic
- Rows for unprovisioned cell fall into `DEFAULT` catch-all partition
- Still works correctly, but forfeits pruning benefit until backfilled properly

**Why PK includes partition columns:**
```sql
PRIMARY KEY (occurrence_id, observed_at, geo_cell_macro)
```
Postgres requires all partition-key columns in PK for multi-level partitioned table.

---

## H3 Implementation

### Postgres Extension: `h3` (h3-pg)

**Implemented via:** `h3` extension (includes PostGIS integration functions)

**Note:** There is no separate `h3_postgis` extension - all PostGIS integration is included in the main `h3` extension.

**Generated columns:**
```sql
-- Resolution 9 for fine-grained occurrence indexing (~0.1 km² cells)
geo_cell_fine H3INDEX GENERATED ALWAYS AS
    (h3_latlng_to_cell(ST_Force2D(location::geometry), 9)) STORED,

-- Resolution 6 for macro-level node ownership (~36 km² cells)
geo_cell_macro H3INDEX GENERATED ALWAYS AS
    (h3_cell_to_parent(h3_latlng_to_cell(ST_Force2D(location::geometry), 9), 6)) STORED,
```

**✅ VERIFIED:** `h3-pg` function signatures have been verified against PGXN documentation.

- Function name: `h3_latlng_to_cell()` (no underscore between "lat" and "lng")
- Function name: `h3_cell_to_parent()` ✅
- All signatures match the installed `postgresql-18-h3` package
- **Documentation:** See [`../technical/h3-pg-extension.md`](../technical/h3-pg-extension.md) for complete function reference.

### Rust Crate: `h3o`

Separate from Postgres extension. Used in Rust code for:
- Computing H3 cells from lat/lon
- H3 parent/child relationships
- H3 neighbors
- Serialization/deserialization

See [`../technical/h3o-crate.md`](../technical/h3o-crate.md) for detailed Rust crate documentation.

---

## Retention & Rate Limiting

### Write-Time Rate Limiting

**10-20s max report frequency per device, per node**

- Enforced at edge, before write or sync
- In-memory/local-cache map: `device_hash → last_seen_at`
- Advertisements within threshold dropped or folded into rolling aggregate
- Not a DB constraint

**Open questions:**
- [ ] Validate 10-20s threshold against real capture volume in Phase 0
- [ ] Make per-node configurable (dense vs. sparse deployment areas)

### Two-Tier Retention

1. **Raw tier:** `occurrences` rows for N days
   - Duration TBD from Phase 0 measured volume
   - Monthly partitions dropped after N days
2. **Rollup tier:** `occurrence_rollups` summary rows
   - Populated by periodic job
   - Keeps long-term storage bounded regardless of raw traffic volume

**Open questions:**
- [ ] Raw-tier retention window (N days) — set from Phase 0 measured volume
- [ ] `pg_partman` (or equivalent) config for automating monthly partition creation and rollup/drop cadence

---

## Indexing Strategy

### Occurrence Indexes

```sql
-- Device identity queries (most common: trace a device's history)
CREATE INDEX idx_occurrence_device_hash ON occurrences (device_hash, observed_at DESC);

-- Node activity queries
CREATE INDEX idx_occurrence_origin_node ON occurrences (origin_node_id, observed_at DESC);
CREATE INDEX idx_occurrence_reporting_node ON occurrences (reporting_node_id, observed_at DESC);

-- H3 geo-indexing (fine and macro resolution)
CREATE INDEX idx_occurrence_geo_fine ON occurrences (geo_cell_fine, observed_at DESC);
CREATE INDEX idx_occurrence_geo_macro ON occurrences (geo_cell_macro, observed_at DESC);

-- PostGIS spatial queries (proximity searches)
CREATE INDEX idx_occurrence_location ON occurrences USING GIST (location);
```

### Node Indexes

```sql
CREATE INDEX idx_node_type ON nodes (node_type);
CREATE INDEX idx_node_owns_geo_cells ON nodes USING GIN (owns_geo_cells);
CREATE INDEX idx_node_status ON nodes (status);
```

---

## Replication / Sync Mechanism

**Application-level batched sync over MQTT**

**NOT Postgres native logical replication:**
- Logical replication assumes relatively stable publisher/subscriber connections
- This network has mix of always-on and intermittent nodes
- Store-and-forward is default assumption

**Dedup:**
- `INSERT ... ON CONFLICT (occurrence_id, observed_at) DO NOTHING`
- Handles both normal peer sync and multi-aggregator relay duplicates

---

## Capacity Planning Assumptions

### Target Density
- **Upper bound:** 50 devices/sq mile
- **Context:** ~250 Meshtastic nodes currently on longFast channel in NYC
- **Stretch goal:** Integration with NYCMesh (not yet scoped)

### Storage Estimation (Phase 0 Validation Needed)
- Raw occurrence size: ~500-1000 bytes per row (including raw payload)
- Rate-limited writes: ~3-6 writes/device/hour at 10-20s threshold
- Example: 1000 devices × 5 writes/hr × 24 hr × 30 days = ~3.6M rows/month
- At 750 bytes/row: ~2.7 GB/month raw

**These are rough estimates — validate against real capture volume in Phase 0.**

---

## Open Questions / TODOs

- [ ] **H3 resolution tuning** — validate res 6 (macro/ownership) and res 9 (fine/indexing) against 50-devices/sq-mile density target once real capture data exists
- [ ] `pg_partman` (or equivalent) config for automating monthly partition creation and rollup/drop cadence
- [ ] Retention window length (raw-tier N days) — set from Phase 0 measured volume, not guessed up front
- [x] **BLIND SPOT RESOLVED:** `h3-pg` extension function signatures verified against PGXN documentation

---

## References

- **Architecture:** [`overview.md`](overview.md)
- **Data Model:** [`data-model.md`](data-model.md)
- **Data Flow:** [`data-flow.md`](data-flow.md)
- **H3o Crate (Rust):** [`../technical/h3o-crate.md`](../technical/h3o-crate.md)
- **Research Topics:** [`../open-questions/research-topics.md`](../open-questions/research-topics.md)
