-- Revert 202608260000_create_node_revocations.sql
--
-- The revocation ledger is deleted along with its audit trail. That is what
-- reverting this migration means, and it is worth being blunt about: a
-- deployment that has ever revoked a node should not run this revert, because
-- the record of that revocation is the only local evidence of it. The three
-- indexes go with the table.
DROP TABLE IF EXISTS node_revocations RESTRICT;
