-- Revert 202607312147_create_bluetooth_occurrences.sql
--
-- This is the one that loses the data: every occurrence row goes with the
-- partitioned parent, along with each monthly partition attached to it
-- (partitions are auto-dependent, so they drop with the parent under
-- RESTRICT — including the ones ensure_occurrence_partitions() created at
-- runtime, which no migration owns).
--
-- occurrence_relays holds the FK to occurrences, so it goes first.
DROP TABLE IF EXISTS occurrence_relays RESTRICT;
DROP TABLE IF EXISTS occurrences RESTRICT;
