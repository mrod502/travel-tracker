# H3o Crate Documentation

The `h3o` library implements the H3 geospatial indexing system for Rust.

**Crate version:** 0.10.0  
**Features:** `serde`, `geo` enabled in project

---

## What is H3?

H3 is a geospatial indexing system using a hexagonal grid that can be (approximately) subdivided into finer and finer hexagonal grids, combining the benefits of a hexagonal grid with S2's hierarchical subdivisions.

### Key Properties

1. **Hierarchical:** Resolutions 0-15, where each cell can have up to 7 children
2. **Hexagonal:** Most cells have 6 neighbors (12 are pentagons at icosahedron vertices)
3. **Discrete:** Converts continuous lat/lon into discrete cell indices
4. **Spatially contiguous:** Nearby cells have similar indices (mostly)

---

## Core Data Types

### `LatLng`

Latitude/longitude coordinate pair.

```rust
use h3o::LatLng;

let loc = LatLng::new(40.6892, -74.0445).unwrap(); // Statue of Liberty
```

**Key methods:**
- `new(lat, lng) -> Option<LatLng>` - Validates range (-90 to 90 lat, -180 to 180 lng)
- `lat() -> f64` - Get latitude in degrees
- `lng() -> f64` - Get longitude in degrees
- `to_cell(resolution) -> CellIndex` - Convert to H3 cell at given resolution

### `CellIndex`

An H3 cell index (the main data type).

```rust
use h3o::{LatLng, Resolution};

let loc = LatLng::new(40.6892, -74.0445).unwrap();
let cell = loc.to_cell(Resolution::Res9);

// Or parse from string
let cell: CellIndex = "892a1072b9fffff".parse().unwrap();
```

**Key methods:**
- `resolution() -> Resolution` - Get cell resolution (0-15)
- `base_cell() -> u8` - Get base cell number (0-121)
- `is_pentagon() -> bool` - Check if this is a pentagon cell
- `parent(resolution) -> CellIndex` - Get parent at coarser resolution
- `children_count(resolution) -> usize` - Get number of children at finer resolution
- `children(resolution) -> Vec<CellIndex>` - Get all child cells
- `grid_disk(k) -> Vec<CellIndex>` - Get all cells within k steps
- `grid_distance(other) -> i64` - Get grid distance to another cell
- `is_neighbor_with(other) -> bool` - Check if cells share an edge
- `area_km2() -> f64` - Get average area in km²
- `boundary() -> CellBoundary` - Get vertex boundary of cell

### `Resolution`

Enum for H3 resolution levels (0-15).

```rust
use h3o::Resolution;

let res = Resolution::Res9;
let area = res.area_km2(); // ~0.1 km² for res 9
```

**Common resolutions:**
| Resolution | Avg Area | Cell Size | Use Case |
|------------|----------|-----------|----------|
| Res 6 | ~36 km² | ~6.5 km | Node ownership / geo-partitioning |
| Res 7 | ~5 km² | ~2.5 km | City-level aggregation |
| Res 8 | ~0.7 km² | ~0.8 km | Neighborhood level |
| Res 9 | ~0.1 km² | ~0.3 km | Occurrence indexing (planned) |
| Res 10 | ~0.02 km² | ~0.1 km | Fine-grained location |

### `DirectedEdgeIndex`

A directed edge between two H3 cells.

```rust
let edge = cell.edge(neighbor);
let origin = edge.origin();
let destination = edge.destination();
```

### `VertexIndex`

A vertex (corner point) of an H3 cell.

---

## Common Patterns

### Pattern 1: Convert Lat/Lon to H3 Cell

```rust
use h3o::{LatLng, Resolution};

fn geo_to_cell(lat: f64, lng: f64, res: Resolution) -> Option<CellIndex> {
    LatLng::new(lat, lng)?.to_cell(res).into()
}

// Usage
let cell = geo_to_cell(40.6892, -74.0445, Resolution::Res9);
```

### Pattern 2: Get Neighboring Cells

```rust
use h3o::{LatLng, Resolution};

let loc = LatLng::new(40.6892, -74.0445).unwrap();
let cell = loc.to_cell(Resolution::Res9);

// Get all cells within 2 steps (13 cells total)
let neighbors = cell.grid_disk(2);
```

### Pattern 3: Hierarchical Traversal

```rust
use h3o::{LatLng, Resolution};

let loc = LatLng::new(40.6892, -74.0445).unwrap();
let fine_cell = loc.to_cell(Resolution::Res9);

// Get parent at res 6
let parent = fine_cell.parent(Resolution::Res6).unwrap();

// Get all children of res 6 cell at res 9
let children = parent.children(Resolution::Res9);
assert_eq!(children.len(), 7); // Each cell has up to 7 children
```

### Pattern 4: Spatial Query (Coverage)

```rust
use h3o::{geom::Tiler, LatLng, Resolution};
use geo::{Polygon, CoordsIter};

// Define a polygon (e.g., city boundary)
let poly: Polygon = /* ... */;

// Get all H3 cells that intersect the polygon
let tiler = Tiler::new();
let cells = tiler.into_coverage(
    poly,
    Resolution::Res9,
    false // strict = false means cells may extend slightly outside polygon
);
```

### Pattern 5: Distance Calculation

```rust
use h3o::{LatLng, Resolution};

let loc1 = LatLng::new(40.6892, -74.0445).unwrap();
let loc2 = LatLng::new(40.7589, -73.9851).unwrap();

let cell1 = loc1.to_cell(Resolution::Res9);
let cell2 = loc2.to_cell(Resolution::Res9);

let distance = cell1.grid_distance(cell2); // Grid steps (hexagon hops)
```

### Pattern 6: Compact Cell Sets

```rust
use h3o::{LatLng, Resolution};

let loc = LatLng::new(40.6892, -74.0445).unwrap();
let parent = loc.to_cell(Resolution::Res6);
let children = parent.children(Resolution::Res9);

// Compact: replace sets of 7 children with their parent
let compacted = CellIndex::compact(&children, Resolution::Res6).unwrap();
assert_eq!(compacted.len(), 1); // All children compacted back to parent
```

---

## Anti-Patterns & Gotchas

### ❌ Anti-Pattern: Assuming Perfect Hexagons

**Problem:** H3 cells are not perfectly uniform due to spherical distortion and pentagons.

**Solution:** Use `cell.area_km2()` for actual area, don't assume all cells same size.

```rust
// ❌ Wrong: Assume all res 9 cells are 0.1 km²
let area = 0.1;

// ✅ Right: Get actual cell area
let area = cell.area_km2();
```

### ❌ Anti-Pattern: Assuming Spatial Locality = Index Proximity

**Problem:** H3 indices don't guarantee that nearby cells have similar indices (especially across icosahedron face boundaries).

**Solution:** Use `grid_disk()` and `grid_distance()` for spatial queries, not index comparison.

```rust
// ❌ Wrong: Compare indices directly
if (cell1 as u64 - cell2 as u64).abs() < 10 {
    // They're nearby
}

// ✅ Right: Use grid distance
if cell1.grid_distance(cell2) <= 1 {
    // They're neighbors
}
```

### ❌ Anti-Pattern: Ignoring Pentagons

**Problem:** 12 base cells are pentagons (at icosahedron vertices). They have 5 neighbors instead of 6, causing distortion.

**Solution:** Check `cell.is_pentagon()` if this matters for your use case (usually doesn't for occurrence tracking).

### ❌ Anti-Pattern: Using Too Fine Resolution

**Problem:** Resolutions 13-15 are extremely small (~meters). Cell count explodes.

**Solution:** Stick to resolutions 6-10 for city-scale tracking.

```rust
// ❌ Wrong: Too many cells
let cells = loc.to_cell(Resolution::Res15); // ~0.000001 km²

// ✅ Right: Reasonable resolution
let cells = loc.to_cell(Resolution::Res9); // ~0.1 km²
```

### ❌ Anti-Pattern: Forgetting Resolution Matters in Comparisons

**Problem:** Comparing cells at different resolutions doesn't work as expected.

**Solution:** Always compare cells at the same resolution, or use hierarchical methods.

```rust
// ❌ Wrong: Different resolutions
let coarse = loc.to_cell(Resolution::Res6);
let fine = loc.to_cell(Resolution::Res9);
let dist = coarse.grid_distance(fine); // Error!

// ✅ Right: Same resolution
let dist = coarse.grid_distance(coarse.parent(Resolution::Res6).unwrap());
```

### ❌ Anti-Pattern: Not Handling Errors

**Problem:** Many methods return `Option` or `Result`.

**Solution:** Always handle errors appropriately.

```rust
// ❌ Wrong: Ignore Option
let parent = cell.parent(Resolution::Res10); // Already at res 9, will be None

// ✅ Right: Handle Option
if let Some(parent) = cell.parent(Resolution::Res6) {
    // Use parent
}
```

---

## Crate Features

### `std` (default)
Enables standard library support, including `std::error::Error` implementations.

### `serde`
Derives serde traits for H3 types (serialization/deserialization).

```rust
use h3o::CellIndex;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
struct Location {
    cell: CellIndex,
}
```

### `geo`
Enables conversion between H3 cells and `geo` crate types (Polygon, etc.).

```rust
use h3o::geom::{Tiler, Solvent};
use geo::Polygon;

// Convert polygon to H3 cells
let tiler = Tiler::new();
let cells = tiler.into_coverage(polygon, Resolution::Res9, false);

// Dissolve H3 cells back to polygon
let solvent = Solvent::new();
let dissolved = solvent.dissolve(cells);
```

---

## H3o vs. h3-pg (Postgres Extension)

**Important:** `h3o` (Rust crate) and `h3-pg` (Postgres extension) are **separate implementations**. They use the same H3 algorithm but have different APIs.

### Rust (h3o)
```rust
use h3o::{LatLng, Resolution};
let cell = LatLng::new(40.6892, -74.0445).unwrap().to_cell(Resolution::Res9);
```

### Postgres (h3-pg)
```sql
SELECT h3_latlng_to_cell(ST_Point(-74.0445, 40.6892), 9);
```

**✅ RESOLVED:** Function signatures verified against PGXN documentation. See [`h3-pg-extension.md`](../technical/h3-pg-extension.md) for complete reference.

---

## Integration with Project Schema

### Planned Usage

1. **Geo-partitioning (res 6)**
   - Node owns `owns_geo_cells` (H3INDEX[])
   - Queries prune on geography
   - Use: `CellIndex::parent(Resolution::Res6)`

2. **Occurrence indexing (res 9)**
   - `geo_cell_fine` column
   - Use: `LatLng::to_cell(Resolution::Res9)`

3. **Spatial queries**
   - Find nearby occurrences
   - Use: `CellIndex::grid_disk(k)`

4. **Node coverage**
   - Find all nodes covering an area
   - Use: `geom::Tiler::into_coverage()`

### Example: Store H3 Cell in Postgres

```rust
use h3o::{LatLng, Resolution};
use sqlx::postgres::PgArguments;
use sqlx::value::Value;

// Convert H3 cell to 64-bit integer for Postgres H3INDEX type
let cell = LatLng::new(40.6892, -74.0445).unwrap().to_cell(Resolution::Res9);
let h3_index: u64 = cell.into();

// Store in database
sqlx::query("INSERT INTO occurrences (geo_cell_fine) VALUES ($1)")
    .bind(h3_index as i64) // Postgres H3INDEX type
    .execute(&pool)
    .await?;
```

---

## References

- **Official H3 docs:** https://h3geo.org/docs/
- **H3o crate docs:** `cargo doc --package h3o --open`
- **H3o source:** https://github.com/adaumault/h3o
- **Project storage plan:** [`../architecture/storage.md`](../architecture/storage.md)
- **Blind spots:** [`../AGENTS.md#blind-spots`](../AGENTS.md#blind-spots)
