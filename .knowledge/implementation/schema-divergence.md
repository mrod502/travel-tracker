# Database Schema Divergence Analysis

Compares the planned schema (`scratch/bundle/docs/data-model.md`, `scratch/bundle/schema/`)
against the implemented schema (`db/src/migrations/`).

**Last updated:** September 2, 2026
**Status:** ⚠️ **Core schema is aligned with the plan. It has never been executed, and it
cannot currently accept a row dated today.**

> The "implemented" columns in the previous revision of this document (2026-08-14) described an
> intermediate schema that no longer exists — `device_address TEXT`, an `address_type` column,
> separate `service_uuids` / `manufacturer_data` / `raw_payload_hex` columns, and `device_hash
> TEXT`. The migrations now use `BYTEA` addresses, raw-byte hashes, and a single unified
> `signal_payload JSONB`, and there is no `address_type` column at all. What follows was
> re-derived from the migration files on 2026-09-02.

**Standing caveat:** no Postgres server, PostGIS, h3-pg, or Docker daemon exists in the dev
container, so not one statement below has been executed. Every "works" here means "reads
correctly", not "ran successfully".

---

## Migration Inventory

| Migration | Contents |
|-----------|----------|
| `202607312127_add_extensions.sql` | `pgcrypto`, `postgis`, `postgis_raster`, `h3` |
| `202607312137_create_bluetooth_occurrence_types.sql` | 7 enums: `signal_type`, `node_type`, `node_status`, `ble_address_type`, `location_source`, `adv_type`, `sync_direction` |
| `202607312146_create_nodes.sql` | `nodes` + 3 indexes |
| `202607312147_create_bluetooth_occurrences.sql` | `occurrences` (time-partitioned), `occurrence_relays`, 2 monthly partitions |
| `202608020206_create_occurrence_indexes.sql` | 5 occurrence indexes, 3 relay indexes |
| `202608020247_create_sync_cursors.sql` | `sync_cursors` |
| `202608260000_create_node_revocations.sql` | `node_revocations` + 3 indexes |

Ordering is coherent: extensions, then types, then tables that reference the types.

---

## Summary

| Aspect | Planned | Implemented | Status |
|--------|---------|-------------|--------|
| `occurrences` unified across signal types | Yes | Yes | ✅ Aligned |
| `node_id` / `origin_node_id` storage | BYTEA (32-byte SHA-256) | BYTEA | ✅ Aligned |
| Signal-specific payload | Flexible | `signal_payload JSONB` | ✅ Aligned, and it superseded three planned columns |
| Partitioning | RANGE(time) + LIST(geo) | RANGE(time) only | ⚠️ Deliberate simplification; geo-sharding delegated to an application layer that does not do it |
| H3 generated columns | res 9 fine, res 6 macro parent | Same | ✅ By inspection; **unexercised** |
| Relay tracking | Separate table | `occurrence_relays` | ✅ Aligned |
| Revocation tracking | RSL scheme (added later) | `node_revocations` | ✅ Aligned with `specifications/revocation-scheme.md` |
| Derived tables (identity, association) | 5 tables | None | ❌ Missing |
| Partition provisioning | Automated | Two hand-written months | ❌ **Broken for the current month** |

---

## `occurrences` — Actual Columns

From `202607312147_create_bluetooth_occurrences.sql`, 22 columns:

| Column | Type | Notes |
|--------|------|-------|
| `occurrence_id` | `UUID NOT NULL DEFAULT uuidv7()` | Requires PostgreSQL 18 for the built-in `uuidv7()` |
| `signal_type` | `signal_type NOT NULL` | Unified-table discriminator |
| `origin_node_id` | `BYTEA NOT NULL REFERENCES nodes(node_id)` | |
| `observed_at` | `TIMESTAMPTZ NOT NULL` | Partition key |
| `observed_at_node_local` | `TIMESTAMPTZ NOT NULL` | Drift auditing |
| `device_address` | `BYTEA` | Raw bytes, **not** TEXT |
| `device_hash` | `BYTEA NOT NULL` | Raw 32 bytes, **not** TEXT hex |
| `advertised_name` | `TEXT` | |
| `adv_type` | `adv_type` | Nullable enum |
| `rssi` | `SMALLINT NOT NULL` | |
| `tx_power` | `SMALLINT` | |
| `signal_payload` | `JSONB NOT NULL DEFAULT '{}'` | Carries what the plan had as `service_uuids`, `manufacturer_data`, `raw_payload_hex` |
| `location` | `GEOGRAPHY(POINT, 4326)` | |
| `alt_m` / `accuracy_m` | `REAL` | |
| `location_source` | `location_source NOT NULL` | No `unknown` value — see D1 below |
| `geo_cell_fine` | `H3INDEX GENERATED … STORED` | res 9 |
| `geo_cell_macro` | `H3INDEX GENERATED … STORED` | res 6, parent of fine |
| `signed_payload` | `BYTEA NOT NULL` | |
| `signature` | `BYTEA NOT NULL` | |
| `schema_version` | `SMALLINT NOT NULL DEFAULT 1` | |
| `ingested_at` | `TIMESTAMPTZ NOT NULL DEFAULT now()` | |

**Table constraints:** `PRIMARY KEY (occurrence_id, observed_at)`, `PARTITION BY RANGE (observed_at)`.
`observed_at` is in the PK because Postgres requires the partition key in the primary key — it
is not a geo-sharding statement, contrary to what an earlier revision of this document implied.

**Deltas from the plan:**
- No `address_type` column, though the `ble_address_type` enum is created. Nothing uses it.
- `service_uuids`, `manufacturer_data`, `raw_payload_hex` collapsed into `signal_payload`.
  This is a better design than the plan for a multi-signal table, and the repo model matches it.
- `rssi` is `NOT NULL`; the plan did not require it. Harmless, but it means a signal type
  without RSSI must invent a value.

---

## `nodes` — Actual Columns

`node_id BYTEA PK`, `node_type`, `mtls_cert_fingerprint` (nullable), `signing_public_key BYTEA
NOT NULL`, `signing_key_algo TEXT DEFAULT 'ed25519'`, `ca_credential BYTEA NOT NULL`,
`fixed_lat` / `fixed_lon DOUBLE PRECISION`, `owns_geo_cells H3INDEX[]`, `registered_at`,
`last_seen_at`, `status node_status DEFAULT 'active'`. Indexes on `node_type`, GIN on
`owns_geo_cells`, and on `status`.

**Deltas from `architecture/overview.md`:** no `display_name` (the cosmetic label the
architecture says exists for logs and UI) and no `short_id` (the compact id reserved for the
LoRa payload budget). Both are described as part of node identity; neither is stored.

---

## D1 — Partition Provisioning Is Broken for Today

```sql
CREATE TABLE occurrences_2026_07 PARTITION OF occurrences FOR VALUES FROM ('2026-07-01') TO ('2026-08-01');
CREATE TABLE occurrences_2026_08 PARTITION OF occurrences FOR VALUES FROM ('2026-08-01') TO ('2026-09-01');
```

Those are the only partitions. The last one ends on 2026-09-01, so as of today
(2026-09-02) **every insert with a current `observed_at` fails** with
`no partition of relation "occurrences" found for row`. The migration comment says
"automate creation via cron/pg_partman in practice"; no code, migration, or operational
script does.

This is the first thing that will break the moment someone runs the migrations and starts a
node, and it will look like an application bug rather than a provisioning gap.

**Two-part fix:** create partitions covering the deployment window now, and add a provisioning
mechanism (scheduled `CREATE TABLE`, or `pg_partman`) so it cannot recur.

---

## D2 — Geo-Partitioning Was Dropped, and Its Replacement Does Not Exist

The plan called for RANGE on time with LIST sub-partitioning on `geo_cell_macro`, so a node
could physically hold only the cells it owns. The implemented schema is RANGE-only, with an
explicit comment that geo-sharding is enforced "at the APPLICATION layer (a node only
ingests/syncs occurrences whose geo_cell it owns)".

No such application logic exists — there is no ingest-side cell-ownership check anywhere in
`app`. That is fine for a single node and must be settled before Phase 1, when the first peer's
data arrives and nothing decides whether this node should accept it.

---

## D3 — `location_source` Has No Honest Value for "No Location"

The enum is `('node_fixed', 'node_gps', 'interpolated', 'aggregator_fixed')` and the column is
`NOT NULL`. A node with no position source at all still has to write one of those four, so
`NoPositionSource` rows assert `node_gps`. A verifier reading the row cannot tell an unlocated
observation from a GPS fix, and the signed payload does not carry the location either, so the
discrepancy is invisible to provenance. See `geo-h3-alignment.md` D4, which proposes adding
`unknown`.

Note `ALTER TYPE ADD VALUE` cannot run inside a transaction block on older Postgres, so it
needs its own migration file — a constraint on the `db` crate's statement splitter.

---

## D4 — H3 Integration Is Correct on Paper and Unproven in Practice

The generated columns are:

```sql
geo_cell_fine  H3INDEX GENERATED ALWAYS AS
    (h3_latlng_to_cell(point(ST_Y(location::geometry), ST_X(location::geometry)), 9)) STORED
geo_cell_macro H3INDEX GENERATED ALWAYS AS
    (h3_cell_to_parent(h3_latlng_to_cell(point(ST_Y(location::geometry), ST_X(location::geometry)), 9), 6)) STORED
```

Two things worth recording, both of which an earlier revision of this document got wrong:

- The expression is **not** `ST_Force2D(location::geometry)` as previously stated. It
  reconstructs a `point(...)` from `ST_Y` then `ST_X`, which encodes the h3-pg v4 argument
  order (latitude first).
- h3-pg v3 took the opposite order and still returns a *valid* cell for transposed input, so a
  deployment with the wrong order fails silently. `repo::geo` now recomputes the same cells
  client-side; comparing the two on every write (`geo-h3-alignment.md` WS4) is what turns that
  silent failure into a loud one. That comparison is not implemented.

Whether `H3INDEX` decodes into `i64` over the wire has never been checked. `repo::geo`
documented `Node::owns_geo_cells` works around it by casting the column to text on read. This
is WS0, and it gates WS2 and WS4.

---

## Invariants the Schema Does Not Enforce

| Invariant | Where it is enforced | Risk |
|-----------|---------------------|------|
| `node_id = SHA-256(signing_public_key)` | Only `app/src/node/identity.rs` | Any other writer can break the self-certifying identity model |
| `node_id` is 32 bytes | Nowhere | Truncated or padded ids insert cleanly |
| `device_hash` is 32 bytes | Only `app/src/node/device_id.rs` | Same |
| `occurrences` is append-only | Only convention ("Never UPDATE") | No trigger blocks UPDATE/DELETE |
| `occurrence_relays.geo_cell_macro` matches the occurrence | Nothing | It is a PK component that can disagree with the row it describes |
| `nodes.status = 'revoked'` follows `node_revocations` | `RevocationRepository::update_node_status` only if that path runs | The two tables can silently disagree |
| `observed_at` is plausible | Nothing | A node with a broken clock writes rows into a partition far in the future |

---

## Missing Tables

| Table | Purpose | Needed by |
|-------|---------|-----------|
| `occurrence_rollups` | Retention/aggregation tier | Retention policy, Phase 6 |
| `device_identities` | Stable device identity | Phase 3 — and `bt_iden` has nowhere to write its results |
| `device_address_links` | Address-to-identity mapping | Phase 3 |
| `co_occurrence_events` | Raw co-location events | Phase 3 |
| `association_edges` | Aggregated device relationships | Phase 3 |

`node_revocations` (2026-08-26) exists and matches the revocation scheme: `node_id`,
`revoked_at`, `revoked_by`, `reason` with a `CHECK (0..6)`, `signing_public_key`,
`ca_credential`, `rsl_sequence_number`, `notes`.

One caution: the migration's reason-code comment maps `PolicyViolation = 4` and
`CeasedOperation = 3`, which is *not* RFC 5280's meaning for those codes. The Rust
`RevocationReason` discriminants must agree with that local mapping; the comment documents the
deviation rather than fixing it.

---

## Indexes

Present: `(device_hash, observed_at DESC)`, `(origin_node_id, observed_at DESC)`,
`(geo_cell_fine, observed_at DESC)`, `(geo_cell_macro, observed_at DESC)`, GiST on `location`;
relay indexes on `reporting_node_id`, `(occurrence_id, observed_at)`, and
`(geo_cell_macro, observed_at DESC)`; `nodes` on type/status plus GIN on `owns_geo_cells`.

Reasonable for the planned query shapes. Note that on a partitioned table these are local
indexes per partition — fine for time-scoped queries, worth revisiting for cross-month device
histories.

---

## What Has to Happen Before the Schema Can Be Called Done

1. Create partitions covering the deployment window, plus a provisioning job.
2. Run all seven migrations against PostgreSQL 18 + PostGIS + h3-pg, and record the h3-pg
   version and `h3index` wire behaviour (WS0).
3. Add the missing `unknown` value to `location_source`.
4. Enforce `node_id = SHA-256(signing_public_key)` and the 32-byte lengths in SQL, not only in Rust.
5. Decide the fate of `ble_address_type`: add the column or drop the enum.
6. Add `display_name` / `short_id` if the architecture is to keep claiming them.

---

## References

- **Migrations:** `db/src/migrations/`
- **Migration tooling:** `db/README.md`, `db/src/runner.rs`
- **Repo-side mapping:** [`repo-divergence.md`](repo-divergence.md)
- **Geo design:** [`geo-h3-alignment.md`](geo-h3-alignment.md)
- **H3 reference:** [`../technical/h3-pg-extension.md`](../technical/h3-pg-extension.md),
  [`../technical/h3o-crate.md`](../technical/h3o-crate.md)
- **Storage design:** [`../architecture/storage.md`](../architecture/storage.md)
- **Deep validation:** [`GAP_ANALYSIS.md`](../../GAP_ANALYSIS.md)
