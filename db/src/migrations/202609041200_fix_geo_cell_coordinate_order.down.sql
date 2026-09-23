-- Revert 202609041200_fix_geo_cell_coordinate_order.sql
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
