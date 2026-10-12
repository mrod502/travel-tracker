# Database Schema Divergence Analysis

Compares the planned schema (`scratch/bundle/docs/data-model.md`, `scratch/bundle/schema/`)
against the implemented schema (`db/src/migrations/`).

**Last updated:** October 12, 2026
**Status:** ✅ **Core schema is aligned with the plan. It has been executed against a real
server, and it accepts a row dated today.**

> The "implemented" columns in the previous revision of this document (2026-08-14) described an
> intermediate schema that no longer exists — `device_address TEXT`, an `address_type` column,
> separate `service_uuids` / `manufacturer_data` / `raw_payload_hex` columns, and `device_hash
> TEXT`. The migrations now use `BYTEA` addresses, raw-byte hashes, and a single unified
> `signal_payload JSONB`, and there is no `address_type` column at all. What follows was
> re-derived from the migration files on 2026-09-02.

**Standing caveat, superseded 2026-10-12.** This document was written in a container with no
database of any kind, and the "never executed" framing outlived that container by six weeks.
PostgreSQL 18.3 with PostGIS 3.6.4 and h3-pg 4.2.3 is reachable at `database:5432`; `db up`
against an empty database applies all 13 migrations, and the generated `h3index` columns, the
composite-partition-key foreign key and `h3_latlng_to_cell` have all been observed running —
see [B2](../../GAP_ANALYSIS.md) and the measured
[`h3index` wire behaviour](ws0-h3index-wire-results.md). Two full workspace test runs against
that server today pass 905 tests over 25 suites. What remains unexercised is named where it
appears: D2's ingest-side cell check, D4's client-side recomparison, and everything that needs
a second node.

---

## Migration Inventory

| Migration | Contents |
|-----------|----------|
| `202607312100_create_migrations_registry.sql` | `migrations` — the apply registry, declared here rather than in application code, and the floor of `db down` |
| `202607312127_add_extensions.sql` | `pgcrypto`, `postgis`, `postgis_raster`, `h3` |
| `202607312137_create_bluetooth_occurrence_types.sql` | 7 enums: `signal_type`, `node_type`, `node_status`, `ble_address_type`, `location_source`, `adv_type`, `sync_direction` |
| `202607312146_create_nodes.sql` | `nodes` + 3 indexes |
| `202607312147_create_bluetooth_occurrences.sql` | `occurrences` (time-partitioned), `occurrence_relays`, 2 monthly partitions |
| `202608020206_create_occurrence_indexes.sql` | 5 occurrence indexes, 3 relay indexes |
| `202608020247_create_sync_cursors.sql` | `sync_cursors` — schema only: no Rust reads or writes this table, and there is no transport to cursor ([B16](../../GAP_ANALYSIS.md)) |
| `202608260000_create_node_revocations.sql` | `node_revocations` + 3 indexes; `revoked_by BYTEA` |
| `202609021200_partition_occurrences_forward.sql` | Extends the partition horizon 18 months; `db up` re-extends it on every deploy |
| `202609041200_fix_geo_cell_coordinate_order.sql` | Rebuilds both generated geo columns with h3-pg v4's latitude-first argument order |
| `202609141353_ensure_occurrence_partition_on_demand.sql` | `ensure_occurrence_partition(ts)`; `OccurrenceRepository::create` calls it and retries when a row lands outside the horizon |
| `202609142142_create_revocation_status_lists.sql` | `revocation_status_lists` — one signed list per `(issuer_id, sequence_number)`, `sequence_number > 0`, `octet_length` CHECKs on the issuer (32) and signature (64) |
| `202609271430_create_device_identity_tables.sql` | `device_identities`, `device_address_links`, `co_occurrence_events`, `association_edges` |

Ordering is coherent: registry, extensions, types, then the tables that reference the types.
Every migration except the registry floor declares a `--migrate:down` block, enforced by
`every_non_floor_migration_declares_a_down_block`. `db/src/migrations/` also holds `PLAN.md`
and `README.md`, which the runner does not treat as migrations.

---

## Summary

| Aspect | Planned | Implemented | Status |
|--------|---------|-------------|--------|
| `occurrences` unified across signal types | Yes | Yes | ✅ Aligned |
| `node_id` / `origin_node_id` storage | BYTEA (32-byte SHA-256) | BYTEA | ✅ Aligned |
| Signal-specific payload | Flexible | `signal_payload JSONB` | ✅ Aligned, and it superseded three planned columns |
| Partitioning | RANGE(time) + LIST(geo) | RANGE(time) only | ⚠️ Deliberate simplification; geo-sharding delegated to an application layer that does not do it |
| H3 generated columns | res 9 fine, res 6 macro parent | Same | ✅ Executed: `h3_latlng_to_cell` returns `892a1072b5bffff` for the fixture coordinate, matching `repo::geo` |
| Relay tracking | Separate table | `occurrence_relays` | ⚠️ Aligned in schema only — no repository and no writer exists ([M7](../../GAP_ANALYSIS.md)) |
| Revocation tracking | RSL scheme (added later) | `node_revocations` + `revocation_status_lists` | ✅ Aligned with `specifications/revocation-scheme.md`, and the list is signed, sequenced and verified on load |
| Derived tables (identity, association) | 5 tables | 4 of 5, created 2026-09-27 and written by `app identity-replay --write` | ⚠️ Only `occurrence_rollups` is missing |
| Partition provisioning | Automated | 18-month horizon extended by `db up`, plus `ensure_occurrence_partition(ts)` and an insert-time retry | ✅ Aligned — see D1 |

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
| `device_address` | `BYTEA` | Raw bytes |
| `device_hash` | `BYTEA NOT NULL` | Raw 32 bytes |
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

## D1 — Partition Provisioning (resolved 2026-09-14)

This section recorded that the only partitions were `occurrences_2026_07` and `_08`, so every
insert dated after 2026-09-01 failed with `no partition of relation "occurrences" found for row`,
and that the migration's "automate creation via cron/pg_partman in practice" comment described
a mechanism nobody had built.

Both halves are closed and neither needed `pg_partman`. `202609021200` opens an 18-month horizon
and `db up` extends it again on every deploy, so the horizon moves with the calendar rather than
being re-authored by hand; `202609141353` adds `ensure_occurrence_partition(ts)`, and
`OccurrenceRepository::create` calls it and retries the insert when a row lands outside what
exists. That second half is what makes the design safe rather than merely scheduled: a missed
deploy degrades to one extra function call on the first insert into a new month, and there is no
`DEFAULT` partition to silently absorb rows that were meant to land somewhere specific.
Pinned by `repo/tests/partition_healing.rs` and tracked as
[B1](../../GAP_ANALYSIS.md).

Retention is the deliberate other half that still does not exist: the provisioning function
comments that it "never drops anything: retention is a separate, deliberate operation", and there
is no `DELETE FROM occurrences`, no `DETACH`, and no `pg_dump` tooling anywhere in the tree — see
§9 Stage 5 and the retention row in *What has to happen* below.

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

## D4 — H3 Integration: Executed and Measured, One Comparison Still Missing

The generated columns are, as they now run on PostgreSQL 18.3 with h3-pg 4.2.3:

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

`H3INDEX`'s wire form has since been measured rather than assumed: `repo::types::H3Index` is the
single wire newtype over `h3o::CellIndex`, announcing `h3index` on the way out and accepting both
binary and text on the way in, and h3o itself exports no database types to compete with it. The
measurements are in [`ws0-h3index-wire-results.md`](ws0-h3index-wire-results.md), and
`repo/tests/wire_types.rs` pins the round trip against a live server ([B3](../../GAP_ANALYSIS.md)).
The text-cast workaround this paragraph described is gone.

---

## Invariants the Schema Does Not Enforce

| Invariant | Where it is enforced | Risk |
|-----------|---------------------|------|
| `node_id = SHA-256(signing_public_key)` | `app/src/node/identity.rs` on write, and now on the peer-verify path — `verify_received_occurrence` refuses a registry row whose key does not hash to the id it is keyed by (`app/src/node/full.rs:1149`) | Not in SQL, so a direct writer can still break it; and the derivation is duplicated in Rust (`ca`'s is `pub(crate)`, [M18](../../GAP_ANALYSIS.md)) |
| `node_id` is 32 bytes | Nowhere — `octet_length` CHECKs exist for the RSL issuer/signature and for `device_address_links`, but not for `nodes.node_id` or `occurrences.origin_node_id` | Truncated or padded ids insert cleanly |
| `device_hash` is 32 bytes | `app/src/node/device_id.rs`, and `valid_link_device_hash` on `device_address_links` only | The `occurrences.device_hash` column itself accepts any length |
| `occurrences` is append-only | Only convention ("Never UPDATE") | No trigger blocks UPDATE/DELETE |
| `occurrence_relays.geo_cell_macro` matches the occurrence | Nothing | It is a PK component that can disagree with the row it describes |
| `nodes.status = 'revoked'` follows `node_revocations` | `RevocationRepository::update_node_status` only if that path runs | The two tables can silently disagree |
| `observed_at` is plausible | Nothing | A node with a broken clock writes rows into a partition far in the future |

---

## Missing Tables

| Table | Purpose | Needed by |
|-------|---------|-----------|
| `occurrence_rollups` | Retention/aggregation tier | Retention policy, Phase 6 — and nothing else, because retention itself is unwritten |

The other four the plan called for exist since 2026-09-27 (`202609271430`): `device_identities`
keyed by `fingerprint_hash`, `device_address_links` keyed so that one identifier answering to two
identities is a *recorded split* rather than a silent pick, `co_occurrence_events`, and
`association_edges`. `app identity-replay --last 7d --write` is what fills them, and
[B12](../../GAP_ANALYSIS.md) records the first measured pass (55 occurrences → 10 identities,
45 merges, mean evidence coverage 0.36).

`node_revocations` (2026-08-26) and `revocation_status_lists` (2026-09-14) match
`specifications/revocation-scheme.md`: `node_id`, `revoked_at`, `revoked_by BYTEA`, `reason` with
a `CHECK (0..6)`, `signing_public_key`, `ca_credential`, `rsl_sequence_number`, `notes`; and one
signed list per `(issuer_id, sequence_number)` with `sequence_number > 0`. `revoked_by` is NULL
until a list publishes the entry, which is the column's documented meaning, not a missing write.

One caution that stands: the migration's reason-code comment maps `PolicyViolation = 4` and
`CeasedOperation = 3`, which is *not* RFC 5280's meaning for those codes. The Rust
`RevocationReason` discriminants agree with that local mapping; the comment documents the
deviation rather than fixing it.

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

Re-run against the tree on 2026-10-12: two of these are done, one was answered differently than
framed, three remain, and two more belong on the list.

1. ~~Create partitions covering the deployment window, plus a provisioning job.~~ **Done
   2026-09-14** — see D1.
2. ~~Run all seven migrations against PostgreSQL 18 + PostGIS + h3-pg, and record the h3-pg version
   and `h3index` wire behaviour (WS0).~~ **Done** — 13 migrations apply cleanly against
   PostgreSQL 18.3 / PostGIS 3.6.4 / h3-pg 4.2.3, and the wire behaviour is recorded in
   [`ws0-h3index-wire-results.md`](ws0-h3index-wire-results.md).
3. **Still open.** `location_source` is still `('node_fixed', 'node_gps', 'interpolated',
   'aggregator_fixed')` with the column `NOT NULL`, so D3's dishonest row remains writable.
   `ALTER TYPE ADD VALUE` cannot run inside a transaction block, so it needs its own migration
   file and `db`'s statement splitter needs to keep that file out of the surrounding transaction.
4. **Partly done.** `octet_length` CHECKs now exist for the RSL issuer (`= 32`) and signature
   (`= 64`) and for `device_address_links` (`device_hash = 32`, `address = 6` or NULL). What is
   still unenforced in SQL: `nodes.node_id`, `occurrences.origin_node_id`, and
   `occurrences.device_hash` all accept any length, and the `node_id = SHA-256(signing_public_key)`
   equality lives only in Rust — on two paths now, but in neither table.
5. **Answered, though not the way this was framed.** `ble_address_type` was neither dropped nor
   added back to `occurrences`: `202609271430` gave it a column on `device_address_links`, which
   is where the identity resolver actually needs it. `occurrences` remains without an address-type
   column, and nothing guesses one.
6. **Still open.** Neither `display_name` nor `short_id` appears in any migration, while
   `architecture/overview.md` still describes both as part of node identity. Either the
   architecture stops claiming them or the table grows them.
7. **New: make relay provenance writeable.** `occurrence_relays` has a model
   (`repo/src/models/occurrence.rs:365`), a primary key of its own, and no repository and no
   `INSERT` anywhere in the tree — so a relayed occurrence cannot record who relayed it
   ([M7](../../GAP_ANALYSIS.md)). The table is currently decoration.
8. **New: retention, partition lifecycle, and an erasure path.** Nothing deletes from
   `occurrences`, nothing detaches a partition, and there is no documented backup or restore
   procedure, so "delete everything about this device" has no implementation and the 18-month
   horizon grows forever. §9 Stage 5 items 22–24; `occurrence_rollups` is the table this needs and
   the last one the plan is missing.

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
