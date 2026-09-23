-- Revert 202608020247_create_sync_cursors.sql
--
-- Replication cursors go with it. Nothing else references this table, so the
-- revert does not depend on ordering — except that it holds the only FK to
-- nodes(other than the occurrences ones), and nodes reverts after it.
DROP TABLE IF EXISTS sync_cursors RESTRICT;
