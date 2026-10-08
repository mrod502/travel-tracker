# Geo/H3 Alignment — Technical Specification

**Date:** 2026-09-01, workstream status refreshed 2026-09-02
**Status:** **Partially implemented** — WS1 and WS2 are in the tree; WS0, WS3, WS4, WS5 are not.
See §5 for the per-workstream state.
**Scope:** three gaps found while tracing the occurrence location path:

1. Node enrollment derives no H3 cell (`nodes.owns_geo_cells` is never written).
   → **closed**: enrollment now derives and stores owned cells.
2. `h3o` is a declared dependency of `repo` and is used nowhere.
   → **closed**: `repo/src/geo.rs` is the client-side implementation.
3. The signed payload does not cover the geo assertion that the database and the
   relay primary key treat as authoritative.
   → **still open**: payload is at `schema_version = 1` with no cell fields.

Everything asserted below about the current tree was read from the code on
2026-09-01. Claims about Postgres/h3-pg runtime behaviour are **unverified in
this container** (no Postgres server, no h3-pg, no Docker daemon) and are marked
as such; WS0 exists to settle them.

---

## 1. Problem surface

### 1.1 Where a coordinate or a cell lives today

| # | Location | Representation | Written by | Read by |
|---|---|---|---|---|
| 1 | `app::position::Position` | `lat: f64, lon: f64, altitude_m: Option<f64>, accuracy_m: Option<f64>, origin, fixed_at` | `PositionSource` impls | `FullNode::store_occurrence` |
| 2 | `PositionConfig` / `GpsSettings` | `fixed: Option<(f64,f64)>` | layered config (`[location].fixed`, `--fixed-location`, `BT_LOCATION_FIXED`) | `build_position_source` |
| 3 | `CanonicalPayload.location` | `Option<[f64; 2]>`, **`[lat, lon]`** | `signed_occurrence` (`node/full.rs`) | CBOR encode/decode, `verify.rs` |
| 4 | `signal_payload.position` (JSON) | `{origin, fixed_at, accuracy_m, altitude_m}` — **no coordinates** | `record_position` (`node/full.rs`) | whoever parses `signal_payload` later |
| 5 | `occurrences.location` | `GEOGRAPHY(POINT,4326)`, encoded `POINT(lon lat)` | `repo` insert via `PostgisPoint` | queries, generated columns |
| 6 | `occurrences.alt_m/accuracy_m/location_source` | `REAL/REAL/enum NOT NULL` | `Occurrence::with_location` | queries |
| 7 | `occurrences.geo_cell_fine/geo_cell_macro` | `H3INDEX GENERATED ALWAYS … STORED` (res 9, res 6) | **Postgres only** | `query --geo-cell`, `idx_occurrence_geo_*` |
| 8 | `nodes.fixed_lat/fixed_lon` | `DOUBLE PRECISION` (lat first) | `node_repo::register` | nothing in Rust at runtime |
| 9 | `nodes.owns_geo_cells` | `H3INDEX[]` + GIN index | `node_repo::register`, derived at enrollment (WS2) | `node_repo::owns_geo_cells`; nothing at capture time |
| 10 | `occurrence_relays.geo_cell_macro` | `H3INDEX NOT NULL`, **part of the PK**, commented "Must match occurrence geo_cell_macro" | nobody (table unused) | nothing |
| 11 | CLI | `--lat/--lon` (enroll), `--fixed-location "lat,lon"`, `query --geo-cell <u64>` | operator | — |

### 1.2 The three inconsistencies that actually matter

**(a) The cell is derived after the fact, in a system the signer does not control.**
`geo_cell_fine`/`geo_cell_macro` are computed by h3-pg at INSERT time. Two nodes
running different h3-pg builds can therefore disagree on the cell for *byte-identical
signed payloads*. The migration comment records the v4 convention
(`h3_latlng_to_cell(point(lat, lng), 9)`, lat first); h3-pg v3 took the opposite
order and **still returns a valid cell for transposed input**, so a wrong-order
deployment fails silently rather than loudly. Nothing in Rust can currently
detect that, because there is no independent implementation to compare against.

**(b) `occurrence_relays.geo_cell_macro` is a primary-key component that nothing validates.**
The schema comment says it must match the occurrence's macro cell. A relay can
insert any value, and a verifier holding only the signed payload cannot check it,
because the payload carries no cell.

**(c) A row can assert a location the signature does not contain.**
`location_source` is `NOT NULL` with no "unknown" variant, so an occurrence stored
without a position still carries `node_gps`. `alt_m`/`accuracy_m` are in the row
but not first-class payload fields (accuracy currently rides inside the
`signal_payload` JSON blob). Fix staleness is invisible to a verifier: a cached
fix up to `[location.gps].max_age_ms` (default 60 000 ms) is presented as the
location *at* `observed_at`.

### 1.3 Representation footgun

Three orderings coexist: payload `[lat, lon]`, `PostgisPoint` `(x=lon, y=lat)`,
WKT `POINT(lon lat)`, `nodes` `fixed_lat, fixed_lon` (lat first). Any new API in
this area must take named parameters, never a bare tuple.

---

## 2. Constraints

- **Migrations only.** Per `AGENTS.md`, no table is edited by hand; all schema
  change goes through the `db` crate CLI. `db/src/up.rs:202` runs each migration
  file **inside a transaction**, splitting statements with `split_query`, which
  respects comments and string literals. Consequences: `ALTER TYPE … ADD VALUE`
  must live in its own migration and must not use the new value in the same file
  (PG12+ rule; target is PostgreSQL 18 per README).
- **`h3index` wire behaviour — measured, see
  [`ws0-h3index-wire-results.md`](ws0-h3index-wire-results.md).** It is a distinct
  pass-by-value base type, **not** a domain over `bigint`, with explicit-only casts.
  sqlx decodes it as neither `i64` nor `String` and rejects a bare-`i64` bind with
  `operator does not exist: h3index = bigint`. So `Occurrence.geo_cell_macro: Option<i64>`
  is broken today, and the strategy is one `H3Index` newtype in `repo::types` that
  encodes the 8-byte big-endian `u64` and decodes both wire formats.
- **Already-signed rows.** Occurrences signed under `schema_version = 1` exist on
  any deployed node. The decoder must keep reading v1; v2 is additive.
- **No Postgres, no gpsd, no Docker in the dev container.** Anything touching SQL
  is verified by `cargo check`/`clippy`/unit tests here and by host-run
  `#[ignore]`d tests elsewhere. That split is enforced by the test plan, not by
  convention.
- **`app` is a bin-only crate**; unused public items surface as dead-code
  warnings, so new helpers must be reachable from the binary or marked
  `#[cfg(test)]`.

---

## 3. Decisions

### D1 — `repo::geo` becomes the single client-side H3 implementation (gap 2)

`h3o` already sits in `repo/Cargo.toml:27` with `features = ["serde", "geo"]`.
Put the conversion in `repo` so the app, the verifier, and any future query tool
share one implementation, and keep `h3o` out of `app`.

Verified against the vendored `h3o-0.10.0` source (`~/.cargo/registry/src/*/h3o-0.10.0`):

```rust
LatLng::new(lat, lng) -> Result<Self, InvalidLatLng>   // const fn — NOT Option
LatLng::to_cell(self, Resolution) -> CellIndex
CellIndex::parent(self, Resolution) -> Option<CellIndex>
Resolution: TryFrom<u8>, From<Resolution> for u8       // variants are Nine/Six, not Res9/Res6
impl From<CellIndex> for u64 ; impl TryFrom<u64> for CellIndex
```

> `.knowledge/technical/h3o-crate.md` currently documents `LatLng::new -> Option<LatLng>`
> and `Resolution::Res9`. Both are wrong for 0.10; WS5 corrects it.

**A cell is an `h3o::CellIndex` everywhere above `repo::geo`.**

> *Amended 2026-09-14.* This section originally specified `i64` as the cell type,
> because `Occurrence::geo_cell_*` was declared `Option<i64>` and `query --cell`
> bound one. That declaration was defect B3, and it is gone: h3o exports no
> database type of its own, so `repo::types::H3Index` is a thin sqlx newtype over
> `h3o::CellIndex` and h3o's type is the currency in every signature. As shipped:

```rust
// repo/src/geo.rs — as shipped 2026-09-14
pub const RESOLUTION_FINE: Resolution = Resolution::Nine;
pub const RESOLUTION_MACRO: Resolution = Resolution::Six;

/// Res-9 cell for a WGS-84 point, as the value Postgres stores.
pub fn fine_cell(lat: f64, lon: f64) -> Result<CellIndex, GeoError>;

/// Res-6 cell, computed as the parent of the res-9 cell so the client mirrors
/// `h3_cell_to_parent(h3_latlng_to_cell(…, 9), 6)` exactly rather than
/// re-deriving at res 6 (which is not the same computation at cell boundaries).
pub fn macro_cell(lat: f64, lon: f64) -> Result<CellIndex, GeoError>;

pub fn cell_from_latlng(lat: f64, lon: f64, resolution: Resolution)
    -> Result<CellIndex, GeoError>;
pub fn parent_cell(cell: CellIndex, resolution: Resolution) -> Result<CellIndex, GeoError>;
pub fn cell_to_latlng(cell: CellIndex) -> (f64, f64);               // cell centre
pub fn parse_cell(raw: &str) -> Result<CellIndex, GeoError>;        // hex or decimal
```

There is no `format_cell` and no `cell_resolution`: `CellIndex`'s own `Display` is
the canonical lowercase-hex spelling h3-pg prints, and `CellIndex::resolution()`
cannot fail once the input is a cell rather than an integer.

`macro_cell` deliberately goes through the res-9 child. h3o's `to_cell(res 6)`
directly from coordinates can differ from `parent(to_cell(res 9), 6)` for points
near a resolution boundary; the SQL uses the parent path, so the client must too.

`GeoError` variants: `InvalidCoordinate { lat, lon }`, `InvalidCell(String)`,
`NoParent { cell, cell_resolution, resolution }` — `thiserror`, matching `repo`'s
existing error style. `InvalidResolution` went with the `u8`: h3o's `Resolution`
has no inhabitant outside 0..=15, so there is nothing left to reject at runtime.

### D2 — Ownership cells are a claim, derived at enrollment and stored explicitly (gap 1)

| Option | Verdict |
|---|---|
| Generated column from `fixed_lat/fixed_lon` | **Rejected.** Ownership is policy, not a function of position: a node may own cells it is not inside, and a mobile node owns none. A generated column would make the claim un-editable. |
| DB trigger | **Rejected.** Policy hidden in the database, no audit trail, untestable here. |
| Client-side derivation at enrollment, explicit column write | **Chosen.** Matches the existing `H3INDEX[]` column and GIN index, keeps the claim inspectable and updatable. |

Rules:

- `ca ca-enroll` with a resolved location and no explicit ownership ⇒ `owns_geo_cells = [macro_cell(lat, lon)]`.
- `--owns-cell <hex|decimal>` (repeatable) overrides entirely; validated
  `Resolution == 6` (a res-9 claim would silently never match `geo_cell_macro`
  queries and is almost certainly a typo).
- `[node] owns_cells = ["862a1072bffffff", …]` in the config file, same layering
  rules as every other setting (flag > file > env > default).
- No location and no explicit cells ⇒ empty array, which is the honest answer for
  a mobile node — **not** an error.
- Enrollment must not fail because a cell could not be computed for an out-of-range
  coordinate: that is a hard error with the coordinate in the message.

`node_repo::register` gains one parameter and one column; binding strategy is
`Vec<i64>` **or** text-array-plus-cast, decided by WS0:

```rust
pub async fn register<'e, E>(
    executor: E, node_id: &[u8], node_type: NodeType,
    signing_public_key: &[u8], ca_credential: &[u8],
    fixed_location: Option<(f64, f64)>,
    owns_geo_cells: &[i64],          // NEW
) -> Result<(), RepoError>
```

The upsert must **not** overwrite `owns_geo_cells` when the caller passes an empty
slice and the row already owns cells — ownership is granted out-of-band and a
credential renewal must not erase it. Use
`owns_geo_cells = COALESCE(NULLIF($7, '{}'), nodes.owns_geo_cells)`.

### D3 — Sign the geo assertion; keep the database's derivation as a cross-check (gap 3)

| Option | Verdict |
|---|---|
| **A.** Drop the generated columns; the app writes cells taken from the signed payload. | Correct end state for Phase 1 (relays), but destructive on any deployed DB and it removes a property other tooling may rely on. **Deferred.** |
| **B.** Payload v2 carries the node's own cells; the DB keeps deriving; every INSERT compares the two. | **Chosen for Phase 0.** No destructive migration; the comparison is free because the insert already `RETURNING *`, so the derived cells come back with the row. Detects h3-pg version/argument-order drift on 100 % of writes instead of during an incident. |

`schema_version = 2`, fields **appended** (the rule in
`roadmap/phase_0/canonical-payload-spec.md` §"Future Versions"):

```text
0..11  unchanged from v1 (9. location: Option<[f64; 2]> stays as [lat, lon])
12  geo_cell_fine      Option<i64>   res 9, computed by the origin node
13  geo_cell_macro     Option<i64>   res 6, parent of 12
14  accuracy_m         Option<f32>
15  alt_m              Option<f32>
16  location_origin    Option<u8>    0 fixed, 1 gps, 2 mock, 3 relay, 4 aggregated
17  fixed_at           Option<String> RFC 3339, the instant of the fix (not of the observation)
```

Invariants the encoder enforces and the decoder re-checks:

- `geo_cell_fine.is_some() == geo_cell_macro.is_some() == location.is_some()`.
- `geo_cell_macro == parent(geo_cell_fine, 6)` — recomputed on decode, because a
  relaying node must be able to prove the pair is self-consistent without
  trusting the origin's arithmetic.
- `location_origin`/`fixed_at` present iff `location` is present.
- `signal_payload.position` is **removed** from the JSON: fields 14–17 are now the
  single source of truth. Leaving both means two signed copies that can diverge.

Decoder: `decode_payload` peeks the leading `schema_version` (uint) and dispatches
`1 => CanonicalPayloadV1`, `2 => CanonicalPayloadV2`, anything else
`Err(UnsupportedVersion)`. `AppConfig` records the version it writes into the
row's existing `schema_version` column. v1 payloads keep verifying as v1 — bytes
of already-stored rows never change.

### D4 — A row with no location must stop claiming one

`ALTER TYPE location_source ADD VALUE 'unknown'` in its own migration (see the
transaction constraint in §2), `Occurrence` builder default flips to `Unknown`,
and `FullNode` sets `NodeFixed`/`NodeGps` only when it actually has a `Position`.
Cross-check on write: location present ⇔ source ≠ `unknown`.

This is the one schema change in the plan that is not additive-only: `unknown`
becomes a value every consumer must handle. It replaces a lie currently written
on every location-less occurrence.

---

## 4. Test plan

### Runs in this container (no database)

- `repo::geo` golden fixtures: one coordinate per case — Statue of Liberty,
  southern hemisphere, near a res 6/9 boundary, the antimeridian (both sides),
  `0,0`, a pole, out-of-range lat/lon ⇒ `InvalidCoordinate`, resolution 16 ⇒
  `InvalidResolution`. Expected values are **generated from h3o once and pinned**,
  then cross-checked against h3-pg on the host (WS0/WS6). Pinning h3o's own output
  is not self-verifying in isolation — its value is that a later h3o upgrade or an
  h3-pg mismatch shows up as a diff.
- `macro_cell` equals `parent(fine_cell, 6)` for every fixture.
- `parse_cell` ↔ `CellIndex::Display` round-trip; decimal and hex input; junk rejected.
- Payload v2: encode→decode round-trip, version detection, self-consistency
  rejection (`macro != parent(fine)`), and v1 fixture bytes still decoding.
- Enrollment: `owns_geo_cells` derivation from a fixed location; `--owns-cell`
  parsing (hex, decimal, res ≠ 6 rejected); config `[node] owns_cells` layering
  under flag > file > env.
- `store_occurrence`: v2 fields populated when a position exists and absent when
  it does not; `location_origin`/`fixed_at` land exactly once (not duplicated into
  `signal_payload`).
- Guard test: no code path yields `{location: None, source: NodeGps}`.

### Host-only, `#[ignore]`d, with the command in the test name

```bash
DATABASE_URL=postgres://… cargo test -p repo -- --ignored geo_probe
DATABASE_URL=postgres://… cargo test -p app  -- --ignored geo_h3_agreement
```

- **WS0 probe:** `h3index` type OID and category (`SELECT typname, typcategory, typbasetype FROM pg_type WHERE typname = 'h3index'`), whether sqlx decodes it as `i64`, whether an `i64` binds back, and whether `ARRAY[…][]::h3index[]` casts from `int8[]` and from `text[]`.
- Golden fixtures re-derived by `h3_latlng_to_cell(point(:lat, :lon), 9)` and compared with the pinned h3o values.
- `nodes.owns_geo_cells` round-trip and a GIN-index lookup (`WHERE :cell = ANY(owns_geo_cells)`, `EXPLAIN` to confirm the index is used).
- The write-path drift check firing on a deliberately inconsistent row.

### Explicit non-goals

No relay/sync writer implementation (nothing writes `occurrence_relays`
today; only the verification helper is added). No change to res 9 / res 6 —
those resolutions and their indexes stay. No HTTP/API surface. No backfill of
pre-v2 rows. No automatic cell-ownership management.

---

## 5. Work breakdown

Statuses verified against the tree on 2026-09-02.

| ID | Deliverable | Depends on | Runs where | Status |
|---|---|---|---|---|
| **WS0** | `h3index` wire-behaviour probe: ignored test + documented psql, result written back into §2 | — | host | ✅ **Done 2026-09-02** — measured on PG 18.3 + PostGIS 3.6.4 + h3-pg 4.2.3. `h3index` is a pass-by-value base type (category `U`, typlen 8) with **explicit-only** casts to/from `bigint`; binary form is the `u64` big-endian, text form is leading-zero-stripped lowercase hex. sqlx decodes it as **neither** `i64` **nor** `String`, so `Occurrence::geo_cell_*: Option<i64}` cannot decode and `SELECT *`/`RETURNING *` fails after the insert. Full results: [`ws0-h3index-wire-results.md`](ws0-h3index-wire-results.md) |
| **WS1** | `repo::geo` module + golden fixtures + unit tests; `h3o` now genuinely used | — | container | ✅ **Done** — `repo/src/geo.rs`, res 9/6 constants, parent-of-fine macro, `parse_cell`/`format_cell` |
| **WS2** | Enrollment derives and stores `owns_geo_cells`; `node_repo::register` param; `--owns-cell`, `[node] owns_cells`, COALESCE-on-renewal | WS1, WS0 | container + host | ✅ **Done in code** — `--owns-cell` (repeatable), `[node] owns_cells`, `BT_OWNS_CELLS`, derivation at enrollment, cells echoed back. Host-side confirmation still owed to WS0 |
| **WS3** | `schema_version = 2`: appended fields, version-dispatching decoder, encoder writes v2, `signal_payload.position` removed | WS1 | container | ❌ **Not started** — every encoder and test still writes version 1; no dispatching decoder |
| **WS4** | Row↔payload consistency: `location_source` `unknown` migration + defaults, drift check against `RETURNING` cells, `[location] strict_cell_check` | WS3, WS0 | container + host | ❌ **Not started** — `location_source` still has no `unknown` value |
| **WS5** | Relay/verifier helper `verify_occurrence_geometry`, `query --cell` hex parsing, `geo cell-of` debug command | WS1, WS3 | container | ⚠️ **Partial** — `geo::parse_cell` exists but is not wired to the CLI. The query flag is actually `query --geo-cell <u64>`, not `--cell <i64>`, and there is no `geo cell-of` command |
| **WS6** | Docs: canonical payload spec → v2, `h3o-crate.md` API corrections, new `.knowledge/technical/h3-consistency.md`, `status.md` entry | WS2–WS5 | container | ⚠️ **Partial** — `status.md` and this file now report the real state; the v2 payload spec edit and `h3-consistency.md` are waiting on WS3 |

Rollout order is WS1 → WS0 → WS2/WS3 → WS4 → WS5 → WS6. WS1 can land first and
independently: it is pure, tested here, and closes gap 2 on its own.

**Where that leaves the plan:** WS1 and WS2 were landed without WS0, which the rollout order
said should come first. That is survivable because both are container-testable and neither
depends on the wire format being right — but it means the first time the code touches a real
database, WS0 becomes blocking rather than advisory, and WS2's stored cells should be treated
as unconfirmed until then.

## 6. Risks

| Risk | Mitigation |
|---|---|
| `h3index` is a distinct base type and sqlx cannot decode it as `i64` — the *existing* `Option<i64>` model field would then be broken too, not just new code | WS0 before WS2/WS4; if it fails, `repo::geo` gains an `H3Index(i64)` newtype with explicit `Decode`/`Encode` in text format, and `Occurrence.geo_cell_*` moves to it |
| h3o and h3-pg disagree (H3 core version, or the v3/v4 argument order) | WS6 agreement test; WS4 drift check on every write means discovery is immediate rather than incident-driven |
| v2 changes signature bytes again | Already accepted by the operator for signed geo fields; v1 rows keep their own version tag and keep verifying; decoder dispatch is tested against pinned v1 bytes |
| `ALTER TYPE ADD VALUE` fails inside the migration transaction | Own migration file, new value unused within it; the runner's statement splitter must see it as one statement — covered by `db` crate parser tests |
| Two nodes disagree on ownership semantics (own-the-cell-you-are-in vs. explicit grant) | §D2 states the rule; enrollment prints the resulting cells; `ca ca-info` reports them |
| Removing `signal_payload.position` breaks anything reading it | Grep confirms no reader today; the fields move to first-class payload positions with the same names |
