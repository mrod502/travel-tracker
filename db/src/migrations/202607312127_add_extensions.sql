--migrate:up.begin
-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS pgcrypto;      -- gen_random_uuid()
CREATE EXTENSION IF NOT EXISTS postgis;       -- geography/geometry types
CREATE EXTENSION IF NOT EXISTS postgis_raster; -- required by postgis in some versions
CREATE EXTENSION IF NOT EXISTS h3;            -- h3-pg: H3 geospatial indexing
                                              -- Includes h3_latlng_to_cell, h3_cell_to_parent,
                                              -- and PostGIS integration functions (h3_cell_to_geometry, etc.)
--migrate:up.end

--migrate:down.begin
-- Revert the statements above.
--
-- RESTRICT, not CASCADE: if anything still depends on these extensions, the
-- migrations that created that something have not been reverted yet, and
-- silently cascading would delete tables nobody asked to delete. The revert
-- fails and says so instead.
DROP EXTENSION IF EXISTS h3 RESTRICT;
DROP EXTENSION IF EXISTS postgis_raster RESTRICT;
DROP EXTENSION IF EXISTS postgis RESTRICT;
DROP EXTENSION IF EXISTS pgcrypto RESTRICT;
--migrate:down.end
