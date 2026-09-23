-- Revert 202608020206_create_occurrence_indexes.sql
--
-- Index-only revert: no rows are lost. Reverting this while the tables still
-- exist costs the queries that need them, which is the point of a revert.
DROP INDEX IF EXISTS idx_occurrence_device_hash;
DROP INDEX IF EXISTS idx_occurrence_origin_node;
DROP INDEX IF EXISTS idx_occurrence_geo_fine;
DROP INDEX IF EXISTS idx_occurrence_geo_macro;
DROP INDEX IF EXISTS idx_occurrence_location;
DROP INDEX IF EXISTS idx_occurrence_relay_reporting_node;
DROP INDEX IF EXISTS idx_occurrence_relay_occurrence;
DROP INDEX IF EXISTS idx_occurrence_relay_geo_cell;
