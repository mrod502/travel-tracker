# Repo Crate Divergence Analysis

Compares the models in `repo/src/models/` against the schema in `db/src/migrations/` and
against the planned data model.

**Last updated:** September 2, 2026
**Status:** ✅ **The historic model/schema misalignment is resolved.** Residual divergences are
listed at the end and are about *coverage*, not shape.

> This document was written on 2026-08-14 against a `BluetoothOccurrence` model that no longer
> exists. Its central finding — "the repo crate models do not match the database schema; this
> is the most significant divergence in the codebase" — is no longer true. The schema was
> refactored onto a unified `Occurrence` model and the Rust models were aligned to it. The
> August analysis is preserved in git history; what follows is the current state.

---

## Summary of the Resolution

| Aspect | Was (2026-08-14) | Now (2026-09-02) |
|--------|------------------|------------------|
| Model name | `BluetoothOccurrence` / table `bluetooth_occurrences` | `Occurrence` / table `occurrences`, `signal_type` discriminator |
| Primary key | `id: Uuid` | `occurrence_id: Uuid` |
| Node identity | `node_id: Uuid` | `origin_node_id: Vec<u8>` — 32-byte SHA-256, `BYTEA` |
| Relay identity | Absent | `OccurrenceRelay { occurrence_id, observed_at, geo_cell_macro, reporting_node_id, ingested_at }` |
| Location | `location_lat` / `location_lon` as `BigDecimal` | `location: Option<PostgisPoint>` over `GEOGRAPHY(POINT,4326)` |
| `service_uuids`, `manufacturer_data`, `raw_payload` | Three typed/`Vec` fields clashing with JSONB | Folded into `signal_payload: serde_json::Value` over `JSONB` |
| `device_hash` | `Option<Vec<u8>>` against a `TEXT` column | `device_hash: Vec<u8>` against `BYTEA NOT NULL` |
| Enums | `Option<String>` for `location_source`, `adv_type`, address type | Typed: `LocationSource`, `AdvType`, `SignalType`, `NodeType`, `NodeStatus` |
| Integer widths | `i32` against `SMALLINT` | `rssi: i16`, `tx_power: Option<i16>`, `schema_version: i16`, `alt_m`/`accuracy_m: Option<f32>` |
| Provenance fields | `signed_payload`, `signature` **missing** | Present, `Vec<u8>`, non-nullable, matching the schema |
| Generated H3 columns | Absent from the model | `geo_cell_fine: Option<i64>`, `geo_cell_macro: Option<i64>` |
| `created_at` vs `ingested_at` | `created_at` | `ingested_at` |
| `observed_at_node_local` | `Option<DateTime<Utc>>` | `DateTime<Utc>` (non-optional, matching `NOT NULL`) |

The `sqlx` mapping problems the August document predicted ("this query will fail against the
actual schema", "cannot implement provenance") no longer apply: `OccurrenceRepository::create`
inserts the unified column set and `RETURNING *`s into `Occurrence`.

**Caveat that has not changed:** the insert has still never been executed. No Postgres,
PostGIS, or h3-pg exists in the dev container, so this alignment is established by reading
both sides, not by a successful query.

---

## Current Repository Surface

| Repository | Methods |
|------------|---------|
| `OccurrenceRepository` | `create`, `find_by_id`, `find_by_device_address`, `find_by_signal_type`, `find_by_geo_cell` |
| `NodeRepository` | `register`, `owns_geo_cells`, `is_registered` |
| `RevocationRepository` | `create`, `is_revoked`, `get_revocation`, `get_all_revoked`, `get_revoked_node_ids`, `get_latest_rsl_sequence`, `update_node_status`, `revoke_node` |

All take `Executor<'_, Database = Postgres>` (except `RevocationRepository`, which takes
`&Pool`), so transactions compose through the same trait — except in the revocation repo,
where the concrete `Pool` signature rules out participating in an outer transaction.

`repo/src/geo.rs` is the client-side H3 implementation: `RESOLUTION_FINE` (9) and
`RESOLUTION_MACRO` (6) as `h3o::Resolution`, `fine_cell`, `macro_cell`,
`cell_from_latlng`, `parent_cell`, `cell_to_latlng`, `parse_cell` — all of them
speaking `h3o::CellIndex` rather than an integer, since B3 closed. `h3o` is a real dependency
in use, not a declared-and-ignored one as every August document recorded.

---

## Resolved Issues

Each issue the August analysis raised, and where it stands:

1. **Node identity model** — resolved. `origin_node_id: Vec<u8>` throughout;
   `reporting_node_id` lives only on `OccurrenceRelay`, exactly as recommended.
2. **Location representation** — resolved via `PostgisPoint` over `GEOGRAPHY(POINT,4326)`;
   PostGIS functions and the GiST index are usable, and generated columns can derive cells.
3. **`service_uuids` typing** — resolved by moving to a single `signal_payload` JSONB column,
   which also serves WiFi and other signal types.
4. **`manufacturer_data` typing** — resolved the same way; the flat one-company assumption is
   gone, the data rides in `signal_payload`.
5. **`device_hash` format** — resolved as raw 32-byte `BYTEA`.
6. **Missing provenance fields** — resolved; all four are modelled and written.
7. **Typed enums** — resolved; `repo/src/models/enums.rs` types are used in the models.
8. **Type widths** — resolved to `i16` / `f32` matching `SMALLINT` / `REAL`.
9. **Generated H3 columns in the model** — resolved.
10. **Table naming** — resolved to `occurrences`.

---

## Residual Divergences

These are the live gaps between the models and the end state.

### R1 — `h3index` wire representation is assumed, not confirmed

`Occurrence.geo_cell_fine/macro` are `Option<i64>` read from `H3INDEX` columns, and
`Node.owns_geo_cells` is `Vec<i64>` read from `H3INDEX[]`. The code says so itself: the field
documented on `Node` notes that reading it back from a `SELECT` goes through
`NodeRepository::owns_geo_cells`, which casts the column to text first, because "decoding an
`h3index[]` straight into `Vec<i64>` assumes a wire representation that has not been confirmed
against a live h3-pg".

This is workstream WS0 in `geo-h3-alignment.md`. If `h3index` is a distinct base type sqlx
cannot decode as `i64`, the existing model fields break too, not just new code. The planned
mitigation is an `H3Index(i64)` newtype with explicit `Encode`/`Decode`. **Highest-risk
unconfirmed assumption in the data layer.**

### R2 — `occurrence_relays` has a model but no repository

`OccurrenceRelay` exists and is exported, with a unit test constructing one. There is no
`RelayRepository`, so nothing can insert or query relay rows. Consistent with Phase 0 scope
(the table is explicitly not needed until Phase 4), but it means the PK invariant "must match
the occurrence's `geo_cell_macro`" has no enforcement point anywhere.

### R3 — No `sync_cursors` model or repository

The table exists; `repo` has nothing for it. Phase 1 cannot start from a standing start here,
but it is a genuine hole relative to the schema.

### R4 — `NodeRepository` cannot support membership

Three methods — `register`, `owns_geo_cells`, `is_registered` — and no way to list peers,
update `last_seen_at`, or change `status`. `RevocationRepository::update_node_status` covers
the revocation case, but nothing else writes those columns. Gossip (Phase 5) and node-registry
sync (Phase 1) both need more than this.

### R5 — `ble_address_type` is defined but unused

The `202607312137` migration creates the `ble_address_type` enum. No column in any table uses
it, and the `Occurrence` model has no address-type field. Either the column gets added or the
enum goes; right now the type system promises something the schema dropped.

### R6 — `mtls_cert_fingerprint` is never populated

Nullable in the schema, modelled on `Node`, never written by `register`. Correct for today
(no mTLS), worth noting so nobody reads a NULL as meaningful.

### R7 — Revocation calls cannot join an outer transaction

`RevocationRepository` methods take `&Pool` rather than a generic `Executor`, unlike the other
two repositories. `revoke_node` needs atomicity across `node_revocations` and `nodes.status`,
which it achieves internally — but it cannot be composed into a larger unit of work.

### R8 — Model documentation drift

`Node::status` is documented as "(active, inactive, revoked)" while `NodeStatus` is
`active | suspected | down | revoked`. Cosmetic, but it is the kind of drift that makes an
operator trust a value that does not exist.

---

## What Works Today

| Operation | Status | Basis |
|-----------|--------|-------|
| `Occurrence` ↔ `occurrences` mapping | ✅ By inspection | Columns and types line up; **never executed** |
| Provenance fields round-trip | ✅ Modelled | `signed_payload`, `signature`, `schema_version`, `origin_node_id` |
| Typed enum mapping | ✅ | `sqlx::Type` derive with `type_name` |
| Client-side H3 derivation | ✅ Tested in container | `repo/src/geo.rs`, h3o |
| PostGIS point encoding | ⚠️ By inspection | `PostgisPoint`; unexercised |
| `H3INDEX` decode | ⚠️ Unconfirmed | See R1 |
| Relay row persistence | ❌ No repository | See R2 |
| Sync cursor persistence | ❌ No model or repository | See R3 |

37 repo tests pass in the container; the 2 failures are `revocation_repo` tests that require
`DATABASE_URL`.

---

## References

- **Implemented models:** `repo/src/models/{occurrence,node,revocation,enums}.rs`
- **Implemented repositories:** `repo/src/repositories/`
- **Client-side H3:** `repo/src/geo.rs`
- **Database schema:** `db/src/migrations/`
- **Schema-side analysis:** [`schema-divergence.md`](schema-divergence.md)
- **Geo design and workstreams:** [`geo-h3-alignment.md`](geo-h3-alignment.md)
- **Deep validation:** [`GAP_ANALYSIS.md`](../../GAP_ANALYSIS.md)
