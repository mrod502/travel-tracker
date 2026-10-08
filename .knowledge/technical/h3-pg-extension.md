# h3-pg PostgreSQL Extension Documentation

**Extension Name:** `h3` (not `h3-pg` or `h3_postgis`)  
**Repository:** https://github.com/postgis/h3-pg (active fork)  
**Original:** https://github.com/zachasme/h3-pg (archived)  
**PGXN:** https://pgxn.org/dist/h3/  
**H3 Core Library:** https://h3geo.org/docs/

---

## Installation

The extension is packaged as `postgresql-16-h3` or `postgresql-18-h3` depending on Postgres version.

```sql
-- Enable the extension (creates all h3_* functions)
CREATE EXTENSION IF NOT EXISTS h3;
```

**Note:** There is no separate `h3_postgis` extension. The PostGIS integration functions are included in the main `h3` extension.

---

## Schema DDL Verification Status

### ✅ VERIFIED: Function Signatures Match

The following functions used in the project schema are **correct**:

```sql
-- ✅ Correct: h3_latlng_to_cell (note: no underscore between 'lat' and 'lng')
h3_latlng_to_cell(point, resolution integer) -> h3index
h3_latlng_to_cell(geometry, resolution integer) -> h3index
h3_latlng_to_cell(geography, resolution integer) -> h3index

-- ✅ Correct: h3_cell_to_parent
h3_cell_to_parent(cell h3index, resolution integer) -> h3index
h3_cell_to_parent(cell h3index) -> h3index
```

### ⚠️ CORRECTION NEEDED: Extension Name

**Current schema uses:**
```sql
CREATE EXTENSION IF NOT EXISTS h3_postgis;  -- ❌ WRONG: No such extension
CREATE EXTENSION IF NOT EXISTS h3;           -- ✅ CORRECT
```

**Migration file:** `/workspace/db/src/migrations/202607312127_add_extensions.sql`

---

## Complete Function Reference

### Indexing Functions (Lat/Lon → H3 Cell)

| Function | Parameters | Return | Description |
|----------|------------|--------|-------------|
| `h3_latlng_to_cell` | `latlng point, resolution integer` | `h3index` | Indexes location at specified resolution |
| `h3_latlng_to_cell` | `geometry, resolution integer` | `h3index` | PostGIS geometry version |
| `h3_latlng_to_cell` | `geography, resolution integer` | `h3index` | PostGIS geography version |
| `h3_cell_to_latlng` | `cell h3index` | `point` | Finds centroid of the cell |
| `h3_cell_to_boundary` | `cell h3index` | `polygon` | Finds boundary polygon of the cell |

**Examples:**
```sql
-- Using point (lon, lat) - PostGIS convention
SELECT h3_latlng_to_cell(POINT(-122.4, 37.8), 9);
-- Result: 8928308280fffff

-- Using geometry
SELECT h3_latlng_to_cell(ST_Point(-122.4, 37.8), 9);

-- Using geography (your schema uses this pattern)
SELECT h3_latlng_to_cell(ST_Force2D(location::geometry), 9);
```

### Hierarchy Functions (Parent/Child)

| Function | Parameters | Return | Description |
|----------|------------|--------|-------------|
| `h3_cell_to_parent` | `cell h3index, resolution integer` | `h3index` | Returns parent at specified resolution |
| `h3_cell_to_parent` | `cell h3index` | `h3index` | Returns parent at next coarser resolution |
| `h3_cell_to_children` | `cell h3index, resolution integer` | `SETOF h3index` | Returns children at specified resolution |

**Examples:**
```sql
-- Get parent at res 6 from res 9 cell
SELECT h3_cell_to_parent('8928308280fffff'::h3index, 6);

-- Nested: convert lat/lng to res 9, then get res 6 parent
SELECT h3_cell_to_parent(
    h3_latlng_to_cell(POINT(-122.4, 37.8), 9),
    6
);
```

### PostGIS Integration Functions

| Function | Parameters | Return | Description |
|----------|------------|--------|-------------|
| `h3_cell_to_geometry` | `h3index` | `geometry` | Convert to PostGIS geometry |
| `h3_cell_to_geography` | `h3index` | `geography` | Convert to PostGIS geography |
| `h3_cell_to_boundary_geometry` | `h3index` | `geometry` | Boundary as geometry |
| `h3_cell_to_boundary_geography` | `h3index` | `geography` | Boundary as geography |

### Common Utility Functions

| Function | Parameters | Return | Description |
|----------|------------|--------|-------------|
| `h3_get_resolution` | `h3index` | `integer` | Returns resolution (0-15) |
| `h3_is_valid_cell` | `h3index` | `boolean` | Returns true for valid H3 cells |
| `h3_cell_area` | `cell h3index, unit text` | `double precision` | Area of cell |
| `h3_get_hexagon_area_avg` | `resolution integer, unit text` | `double precision` | Average hexagon area |

---

## H3 Resolution Reference

| Resolution | Avg Area | Cell Size | Use Case |
|------------|----------|-----------|----------|
| Res 6 | ~36 km² | ~6.5 km | Node ownership / geo-partitioning |
| Res 9 | ~0.1 km² | ~0.3 km | Occurrence indexing |

---

## Schema DDL Examples

### Corrected Extension Declaration

```sql
-- ✅ CORRECT
CREATE EXTENSION IF NOT EXISTS h3;
-- ❌ WRONG: CREATE EXTENSION IF NOT EXISTS h3_postgis;
```

### Generated Columns (as used in your schema)

```sql
-- Resolution 9 for fine-grained occurrence indexing (~0.1 km² cells)
geo_cell_fine H3INDEX GENERATED ALWAYS AS
    (h3_latlng_to_cell(ST_Force2D(location::geometry), 9)) STORED,

-- Resolution 6 for macro-level node ownership (~36 km² cells)
geo_cell_macro H3INDEX GENERATED ALWAYS AS
    (h3_cell_to_parent(h3_latlng_to_cell(ST_Force2D(location::geometry), 9), 6)) STORED,
```

**Note:** The function name is `h3_latlng_to_cell` (no underscore between "lat" and "lng"), NOT `h3_lat_lng_to_cell`.

---

## Data Types

### `h3index`
The main H3 index type. Can be cast to/from other types:

```sql
-- String representation (hex)
'8928308280fffff'::h3index

-- Cast to bigint
SELECT '8928308280fffff'::h3index::bigint;

-- Cast from bigint
SELECT 6442481055078875135::bigint::h3index;

-- Cast to point (centroid)
SELECT '8928308280fffff'::h3index::point;
```

---

## Migration Issues Found

### Issue 1: Wrong Extension Name
**File:** `/workspace/db/src/migrations/202607312127_add_extensions.sql`

**Current (incorrect):**
```sql
CREATE EXTENSION IF NOT EXISTS h3;            -- Correct
CREATE EXTENSION IF NOT EXISTS h3_postgis;    -- WRONG - no such extension
```

**Should be:**
```sql
CREATE EXTENSION IF NOT EXISTS h3;  -- This includes PostGIS integration functions
```

### Issue 2: Function Name Mismatch
**File:** `/workspace/db/src/migrations/202607312147_create_bluetooth_occurrences.sql`

**Current (incorrect):**
```sql
h3_lat_lng_to_cell(...)  -- WRONG: has extra underscore
```

**Should be:**
```sql
h3_latlng_to_cell(...)   -- CORRECT: no underscore between 'lat' and 'lng'
```

---

## Action Items

- [ ] Fix `db/src/migrations/202607312127_add_extensions.sql` - remove `h3_postgis` extension line
- [ ] Fix `db/src/migrations/202607312147_create_bluetooth_occurrences.sql` - change `h3_lat_lng_to_cell` to `h3_latlng_to_cell`
- [ ] Update documentation to reflect verified function signatures

---

## References

- **PGXN Documentation:** https://pgxn.org/dist/h3/docs/api.html
- **GitHub (PostGIS fork):** https://github.com/postgis/h3-pg
- **H3 Core Library:** https://h3geo.org/docs/
- **Debian Package:** `postgresql-18-h3`

---

**Last Updated:** 2026-08-13  
**Verification Status:** ✅ Complete - All function signatures verified against PGXN documentation
