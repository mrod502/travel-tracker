---
name: h3-geospatial-indexing
description: H3 geospatial indexing with h3o (Rust) and h3-pg (Postgres)
source: custom
---

# H3 Geospatial Indexing

**What is H3?** A geospatial indexing system using a hexagonal grid with hierarchical resolutions (0-15). Each cell can have up to 7 children, combining hexagonal grid benefits with S2's hierarchical subdivision.

## When to Use

- **Storing location data** for occurrences/sensor readings in Postgres
- **Spatial queries** (finding nearby devices, grid disks, neighbors)
- **Geo-partitioning** database tables by geographic region
- **Node ownership** tracking which node controls which geographic area
- **Hierarchical aggregation** (coarse res 6 for ownership, fine res 9 for occurrences)

## Procedure

### 1. Database Setup

```sql
-- Enable the H3 extension (includes PostGIS integration)
CREATE EXTENSION IF NOT EXISTS h3;

-- Create H3INDEX column for occurrence storage
CREATE TABLE occurrences (
    -- Location with PostGIS geography type
    location GEOGRAPHY(POINT, 4326),
    
    -- Generated columns (auto-computed from location)
    geo_cell_fine H3INDEX GENERATED ALWAYS AS
        (h3_latlng_to_cell(ST_Force2D(location::geometry), 9)) STORED,
    
    geo_cell_macro H3INDEX GENERATED ALWAYS AS
        (h3_cell_to_parent(
            h3_latlng_to_cell(ST_Force2D(location::geometry), 9),
            6
        )) STORED
);

-- Indexes for spatial queries
CREATE INDEX idx_occurrence_geo_fine ON occurrences (geo_cell_fine, observed_at DESC);
CREATE INDEX idx_occurrence_geo_macro ON occurrences (geo_cell_macro, observed_at DESC);
CREATE INDEX idx_occurrence_location ON occurrences USING GIST (location);
```

### 2. Rust: Convert Lat/Lon to H3 Cell

```rust
use h3o::{LatLng, Resolution};

// Create location and convert to H3 cell
let loc = LatLng::new(40.6892, -74.0445).unwrap(); // lat, lng
let cell = loc.to_cell(Resolution::Res9);

// Get parent at coarser resolution
let parent = cell.parent(Resolution::Res6).unwrap();

// Store in Postgres as u64
let h3_index: u64 = cell.into();
```

### 3. Rust: Get Neighboring Cells

```rust
// Get all cells within k steps (grid disk)
let neighbors = cell.grid_disk(2); // Returns 13 cells for k=2

// Check if two cells are neighbors
let is_neighbor = cell.is_neighbor_with(other_cell);

// Calculate grid distance
let distance = cell.grid_distance(other_cell);
```

### 4. Rust: Hierarchical Traversal

```rust
// Get children of a coarse cell at finer resolution
let children = parent.children(Resolution::Res9);
assert_eq!(children.len(), 7); // Each cell has up to 7 children

// Compact child cells back to parent
let compacted = CellIndex::compact(&children, Resolution::Res6).unwrap();
assert_eq!(compacted.len(), 1); // All 7 children compacted to parent
```

### 5. Postgres: Spatial Queries

```sql
-- Get parent from cell
SELECT h3_cell_to_parent('8928308280fffff'::h3index, 6);

-- Get children of cell
SELECT h3_cell_to_children('862830800ffffff'::h3index, 9);

-- Convert cell to geometry (polygon boundary)
SELECT h3_cell_to_boundary_geometry('8928308280fffff'::h3index);

-- Get centroid as point
SELECT h3_cell_to_latlng('8928308280fffff'::h3index);

-- Check if cell is pentagon (distorted edge case)
SELECT h3_get_resolution('8928308280fffff'::h3index);
```

### 6. Rust: Storage Integration

```rust
// Convert H3 cell to i64 for Postgres binding
let cell = LatLng::new(40.6892, -74.0445).unwrap().to_cell(Resolution::Res9);
let h3_index = cell.into() as i64;

// Insert into database
sqlx::query("INSERT INTO occurrences (geo_cell_fine, location) VALUES ($1, ST_MakePoint($2, $3))")
    .bind(h3_index)
    .bind(-74.0445) // lng
    .bind(40.6892)  // lat
    .execute(&pool)
    .await?;
```

### 7. Spatial Query: Coverage from Polygon

```rust
use h3o::geom::Tiler;
use geo::Polygon;

// Get all H3 cells covering a geographic polygon
let tiler = Tiler::new();
let cells = tiler.into_coverage(polygon, Resolution::Res9, false);

// Find all nodes covering this area
let covering_nodes = /* query nodes where owns_geo_cells overlaps with cells */;
```

## Pitfalls

### ❌ Wrong Extension Name
**Problem:** There is NO `h3_postgis` extension.
**Solution:** Use `CREATE EXTENSION h3;` — the PostGIS integration is included in the main `h3` extension.

### ❌ Wrong Function Name
**Problem:** Function is `h3_latlng_to_cell` (no underscore between "lat" and "lng").
**Solution:** Use `h3_latlng_to_cell(geometry, resolution)` not `h3_lat_lng_to_cell`.

### ❌ Assuming Perfect Hexagons
**Problem:** H3 cells are not perfectly uniform due to spherical distortion and 12 pentagon cells.
**Solution:** Use `cell.area_km2()` for actual area calculations, don't assume uniform size.

### ❌ Assuming Index Proximity = Spatial Proximity
**Problem:** H3 indices don't guarantee nearby cells have similar indices (especially across icosahedron face boundaries).
**Solution:** Use `grid_disk()` and `grid_distance()` for spatial queries, not integer comparison.

### ❌ Using Too Fine Resolution
**Problem:** Resolutions 13-15 are extremely small (~meters), causing cell count explosion.
**Solution:** Stick to resolutions 6-10 for city-scale tracking:
- Res 6: ~36 km² for node ownership/geo-partitioning
- Res 9: ~0.1 km² for occurrence indexing

### ❌ Comparing Cells at Different Resolutions
**Problem:** `grid_distance()` and comparisons require same resolution.
**Solution:** Always use `parent()` or `children()` to normalize resolution before comparing.

### ❌ Ignoring Pentagons
**Problem:** 12 base cells are pentagons (at icosahedron vertices) with 5 neighbors instead of 6.
**Solution:** Check `cell.is_pentagon()` if this matters for your use case.

### ❌ Not Handling Options/Results
**Problem:** Methods like `parent()` return `Option`, many return `Result`.
**Solution:** Always handle errors appropriately with `if let` or `?` operator.

## Resolution Reference

| Resolution | Avg Area | Cell Size | Project Use Case |
|------------|----------|-----------|------------------|
| Res 6 | ~36 km² | ~6.5 km | Node ownership, geo-partitioning |
| Res 7 | ~5 km² | ~2.5 km | City-level aggregation |
| Res 8 | ~0.7 km² | ~0.8 km | Neighborhood level |
| Res 9 | ~0.1 km² | ~0.3 km | Occurrence indexing |
| Res 10 | ~0.02 km² | ~0.1 km | Fine-grained location |

## References

- **H3o Rust docs:** `cargo doc --package h3o --open`
- **H3-pg docs:** https://pgxn.org/dist/h3/docs/api.html
- **H3 official:** https://h3geo.org/docs/
- **Project storage:** See `docs/architecture/storage.md`
