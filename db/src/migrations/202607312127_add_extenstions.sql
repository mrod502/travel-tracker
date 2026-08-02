-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS pgcrypto;      -- gen_random_uuid()
CREATE EXTENSION IF NOT EXISTS postgis;       -- geography/geometry types
CREATE EXTENSION IF NOT EXISTS postgis_raster; -- required by postgis in some versions
CREATE EXTENSION IF NOT EXISTS h3;            -- h3-pg: H3 geospatial indexing
CREATE EXTENSION IF NOT EXISTS h3_postgis;    -- bridges h3 <-> postgis geometry/geography types
