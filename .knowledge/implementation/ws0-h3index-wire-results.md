# WS0 — `h3index` wire behaviour, measured

**Status:** ✅ **Resolved.** Every claim below was observed on 2026-09-02 against a running
server; nothing here is argued from h3-pg documentation.

This closes the open item that gated `geo-h3-alignment.md` WS2/WS4, `§2` of
[`gap_analysis/REPO_MODELS_AND_REPOSITORIES.md`](../../gap_analysis/REPO_MODELS_AND_REPOSITORIES.md),
and `schema-divergence.md`'s "wire behaviour unconfirmed".

## Where it was measured

| | |
|---|---|
| Server | PostgreSQL 18.3 (`Debian 18.3-1.pgdg13+1`), host `database:5432` |
| Extensions | PostGIS 3.6.4, `postgis_raster` 3.6.4, **h3-pg 4.2.3**, pgcrypto 1.4 |
| Database | `travel` (created for this run; `db up` applied all 7 migrations, 2026-09-02) |
| Client | `sqlx` 0.9.0 / `sqlx-postgres` 0.9.0, `h3o` 0.10 |

`db up` succeeding is itself news: it is the first time any SQL in this repository has
executed. `\d occurrences` shows both `GENERATED ALWAYS AS … STORED` H3 columns surviving,
all five indexes accepted on the partitioned parent, and
`fk_occurrence_relays_occurrence (occurrence_id, observed_at) → occurrences(occurrence_id, observed_at)`
accepted against the partitioned table — the composite-partition-key FK that
`MIGRATION_TOOL_AND_SCHEMA.md` §1.3 listed as unknown is valid here.

## The type

```
 oid  | typname  | typtype | typcategory | typlen | typinput   | typoutput   | typreceive   | typsend
------|----------|---------|-------------|--------|------------|-------------|--------------|------------
29009 | h3index  | b       | U           | 8      | h3index_in | h3index_out | h3index_recv | h3index_send
29014 | _h3index | b       | A           | -1     | array_in   | array_out   | array_recv   | array_send
```

A pass-by-value **base type** in category `U` (user-defined) — *not* a domain over `bigint`.

### Casts (from `pg_cast`)

| source | target | context | method |
|--------|--------|---------|--------|
| `bigint` | `h3index` | **`e` (explicit)** | function |
| `h3index` | `bigint` | **`e` (explicit)** | function |
| `h3index` | `point` | `e` (explicit) | function |

**There is no implicit cast.** That single fact decides most of the rest.

## Wire forms, read off a live column

`h3_latlng_to_cell(point(40.6892, -74.0445), 9)`:

| rendering | value |
|-----------|-------|
| text (`h3index_out`) | `89f058792d3ffff` |
| `::bigint` | `621221353442508799` |
| binary (`h3index_send`) | `08 9f 05 87 92 d3 ff ff` |

So: **the binary representation is the `u64` cell index, 8 bytes, big-endian**, and the text
representation is lowercase hex with **no leading zeros and no `0x`**.
`h3o::CellIndex`'s `Display` — which is what `repo` prints a cell as, and what
`repo::types::H3Index` forwards to — is byte-for-byte what `h3index_out` prints, so
the "matching what h3-pg prints" claim is verified rather than hoped.
`h3index_in` accepts both the hex form and the decimal form, and so does
`repo::geo::parse_cell`.

## What sqlx 0.9 actually does with it

Observed on `SELECT geo_cell_fine … FROM occurrences`:

```
type_info = PgTypeInfo(Custom(PgCustomType { oid: 29009, name: h3index, kind: Simple }))
format    = Binary
raw bytes = [08, 9f, 05, 87, 92, d3, ff, ff]
```

and every ordinary decode is rejected:

| attempted decode | result |
|------------------|--------|
| `Option<i64>` | ✗ `mismatched types; Rust type Option<i64> (as SQL type INT8) is not compatible with SQL type h3index` |
| `Option<String>` | ✗ same shape, `TEXT` vs `h3index` |
| `Option<Vec<u8>>` | ✗ same shape, `BYTEA` vs `h3index` |

**Consequence, now proven rather than predicted:** `Occurrence::geo_cell_fine: Option<i64>`
cannot decode. `OccurrenceRepository::create()` — `INSERT … RETURNING *` — therefore fails
**after the row has been inserted**, and all four `find_*` reads fail too. This is defect B3
in the consolidated register, upgraded from 📖 to ✅.

### Bind forms the server accepts

| bind | SQL | result |
|------|-----|--------|
| `i64` | `WHERE geo_cell_macro = $1` | ✗ `operator does not exist: h3index = bigint` |
| `String` (hex) | `WHERE geo_cell_macro = $1` | ✗ `operator does not exist: h3index = text` |
| `String` (hex) | `WHERE geo_cell_macro = $1::h3index` | ✗ `incorrect binary data format in bind parameter 1` |
| `i64` | `WHERE geo_cell_macro = $1::h3index` | ✓ |
| `String` (hex) | `WHERE $1::h3index[] && owns_geo_cells` | ✓ |
| **`H3Index` newtype** (below) | `WHERE geo_cell_macro = $1` | ✓ |

The two forms the product uses today are in the failing rows: `occurrence_repo::find_by_geo_cell`
and the CLI's geo query bind a bare `i64`; `node_repo::register` gets through only because the
array cast happens to be accepted where the scalar one is not.

### The newtype that works

`Type` + `PgHasArrayType` + `Encode` (8-byte big-endian) + `Decode` (binary BE, text as a
fallback) was verified end to end:

| operation | result |
|-----------|--------|
| bind into `= $1` with **no cast in the SQL** | ✓ (matches the operator, so the index is usable) |
| `SELECT geo_cell_macro` → newtype | ✓ correct `i64` |
| `SELECT NULL::h3index` → `Option<_>` | ✓ `None` |
| `Vec<newtype>` ↔ `h3index[]` (`&&`, and reading `owns_geo_cells` back) | ✓ both directions |

That is the strategy `repo` now uses; see `repo::types::H3Index`.

## Found on the way, not in the register: `geography` is broken the same way

`PostgisPoint` encodes WKT and decodes with `value.as_str()`. sqlx asks the server for
**binary**, and PostGIS sends geography as EWKB, so both directions fail:

```
type_info = Custom { oid: 28071, name: geography }   format = Binary
bytes     = 01 | 01 00 00 20 | e6 10 00 00 | <f64 lon LE> | <f64 lat LE>
              ^ little-endian  ^ type 1 (Point) + SRID flag   ^ SRID 4326
decode → Err("invalid utf-8 sequence of 1 bytes from index 5")
encode → Err("Invalid endian flag value encountered")
```

That layout is standard EWKB for a POINT with an embedded SRID: 1 endianness byte, 4-byte
type word (`0x20000000` = SRID present, `1` = Point), 4-byte SRID, then `x = lon` and `y = lat`
as little-endian `f64`.

This is **worse than the `h3index` defect** in one respect: `h3index` breaks only the geo
reads, while `location` breaks every `SELECT *` on `occurrences`, including `create()`. The
Stage 1 exit criterion ("a run writes rows that `app query` returns") is unreachable until
both custom types speak their real wire format. Encoding as WKT against a binary-format
parameter fails loudly, which is the good case — it can't store a wrong location silently.

## Confirmed alongside it

- **B1 is real, with its error text.** `INSERT … observed_at = now()` on 2026-09-02:
  `no partition of relation "occurrences" found for row; Partition key of the failing row
  contains (observed_at) = (2026-09-02 21:52:38.883752+00)`.
- A `GIN (owns_geo_cells)` index on an `h3index[]` column is accepted, so `h3index` has a
  usable btree opclass.
- `nodes.status`, the seven enums, and `node_revocations` all round-trip their DDL.

## Found while closing Stage 1: the generated columns had their coordinates swapped

Not in the register, and not visible from any of the above — the columns exist, are of type
`h3index`, decode correctly, and every one of them named a cell the row is not in.

`h3_latlng_to_cell(latlng point, resolution)` takes the PostGIS point as
**`(x = longitude, y = latitude)`** — the same order `h3_cell_to_latlng` hands back — despite
its name saying `latlng`. The migration read the name and passed
`point(ST_Y(location::geometry), ST_X(location::geometry))`.

Measured on the same server, with the Statue of Liberty (40.6892, −74.0445):

```sql
SELECT point(-74.0445, 40.6892) <@ h3_cell_to_boundary('892a1072b5bffff');  -- t
SELECT point(-74.0445, 40.6892) <@ h3_cell_to_boundary('89f058792d3ffff');  -- f
```

`892a1072b5bffff` is the res 9 cell `h3o` computes for that location. `89f058792d3ffff` —
what the transposed expression produced — is `h3_cell_to_latlng` of a point in the Southern
Ocean roughly 1 600 km south-west of Australia. Latitude and longitude are not symmetric, so
swapping them does not degrade the answer, it answers a different question.

The reach of it:

- every `geo_cell_fine`/`geo_cell_macro` written before this fix is wrong, silently;
- `find_by_geo_cell` could never return the rows for the cell they claim to be in;
- `app ca ca-enroll --lat --lon` derives a res 6 ownership claim with `h3o` (correct order)
  while the rows were filed under the transposed cell, so an aggregator asking "what happened
  in your cell" would have been told nothing, forever.

Fixed by `db/src/migrations/202609041200_fix_geo_cell_coordinate_order.sql`, which rebuilds
both columns from `point(ST_X(location::geometry), ST_Y(location::geometry))`. Pinned by
`repo/tests/wire_types.rs::the_stored_fine_cell_actually_contains_the_location`, which asks
the database rather than the code whether the stored cell contains the stored location.
