--migrate:up.begin
-- =====================================================================
-- Fix the generated H3 columns: they were computed from transposed
-- coordinates.
--
-- 202607312147_create_bluetooth_occurrences.sql wrote
--
--   h3_latlng_to_cell(point(ST_Y(location::geometry), ST_X(...)), 9)
--
-- with the comment "h3-pg v4+ expects point(lat, lng) - note: lat first,
-- then lng". It does not. h3-pg v4.2.3 follows the PostGIS convention it
-- is documented against: the "latlng" parameter is a PostGIS point, so it
-- is (x = longitude, y = latitude) — the same order h3_cell_to_latlng
-- returns. Measured on the live server (PostgreSQL 18.3 / h3-pg 4.2.3) on
-- 2026-09-04, against the cells repo::geo pins for the same coordinates:
--
--   h3_latlng_to_cell(point(151.2153, -33.8568), 9)   -- (lng, lat)
--     = 620336640800063487   ==  h3o fixture for Sydney, 151.2153 E 33.8568 S
--   h3_latlng_to_cell(point(-74.0445, 40.6892), 9)    -- (lng, lat)
--     = 617733151067471871   ==  h3o fixture for the Statue of Liberty
--   h3_latlng_to_cell(point(40.6892, -74.0445), 9)    -- (lat, lng): what the
--     = 621221353442508799      schema was doing, and its own reverse
--                                agrees it is the wrong place:
--                                h3_cell_to_latlng of that cell is
--                                (40.683, -74.044) read as (lng, lat),
--                                i.e. 40 E / 74 S, in the Southern Ocean.
--
-- So every stored geo_cell_fine / geo_cell_macro names a cell that the
-- occurrence is not in, and the two spellings of a cell never meet: the CLI
-- claims ownership using repo::geo (correct) and queries with it, while the
-- rows carry the transposed cell. The consequence is not "slightly wrong
-- cells", it is that a geo query returns nothing for the area a node covers,
-- and node ownership and occurrence location are unrelated numbers.
--
-- Postgres has no ALTER ... SET EXPRESSION for a generated column, so this
-- drops and re-adds. Dependent indexes go with the columns and are rebuilt
-- here exactly as 202608020206_create_occurrence_indexes.sql created them.
-- Existing rows are recomputed on the way through, so the stored cells become
-- the cells the locations actually fall in.
--
-- The stored location itself was always correct — GEOGRAPHY(POINT, 4326) is
-- (lon, lat) and PostGIS reports it that way. Only the derivation was wrong.
-- =====================================================================

ALTER TABLE occurrences
    DROP COLUMN geo_cell_fine,
    DROP COLUMN geo_cell_macro;

ALTER TABLE occurrences
    ADD COLUMN geo_cell_fine H3INDEX GENERATED ALWAYS AS
        (h3_latlng_to_cell(point(ST_X(location::geometry), ST_Y(location::geometry)), 9)) STORED,
    ADD COLUMN geo_cell_macro H3INDEX GENERATED ALWAYS AS
        (h3_cell_to_parent(
            h3_latlng_to_cell(point(ST_X(location::geometry), ST_Y(location::geometry)), 9),
            6
        )) STORED;

CREATE INDEX idx_occurrence_geo_fine  ON occurrences (geo_cell_fine, observed_at DESC);
CREATE INDEX idx_occurrence_geo_macro ON occurrences (geo_cell_macro, observed_at DESC);
--migrate:up.end

--migrate:down.begin
-- Revert the statements above.
--
-- WARNING: this restores a derivation that is known to be wrong.
--
-- The migration above corrected h3_latlng_to_cell() calls that passed
-- (lat, lng) where h3-pg wants the PostGIS (x = lng, y = lat) order. Reverting
-- it puts the transposed expression back, so every stored geo_cell_fine and
-- geo_cell_macro again names a cell the occurrence is not in, and geo queries
-- return nothing for the area a node covers.
--
-- It is here because a revert should be able to reproduce the previous schema
-- exactly, not because reverting this migration is ever the right move on a
-- database with rows in it. Postgres has no ALTER ... SET EXPRESSION for a
-- generated column, so the columns are dropped and re-added either way; the
-- indexes on them go with the columns and are rebuilt below, matching
-- 202608020206_create_occurrence_indexes.sql.
ALTER TABLE occurrences
    DROP COLUMN geo_cell_fine,
    DROP COLUMN geo_cell_macro;

ALTER TABLE occurrences
    ADD COLUMN geo_cell_fine H3INDEX GENERATED ALWAYS AS
        (h3_latlng_to_cell(point(ST_Y(location::geometry), ST_X(location::geometry)), 9)) STORED,
    ADD COLUMN geo_cell_macro H3INDEX GENERATED ALWAYS AS
        (h3_cell_to_parent(
            h3_latlng_to_cell(point(ST_Y(location::geometry), ST_X(location::geometry)), 9),
            6
        )) STORED;

CREATE INDEX idx_occurrence_geo_fine  ON occurrences (geo_cell_fine, observed_at DESC);
CREATE INDEX idx_occurrence_geo_macro ON occurrences (geo_cell_macro, observed_at DESC);
--migrate:down.end
