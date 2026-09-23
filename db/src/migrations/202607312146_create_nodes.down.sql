-- Revert 202607312146_create_nodes.sql
--
-- Drops the node registry, including every row in it. idx_node_type,
-- idx_node_owns_geo_cells and idx_node_status go with the table.
-- sync_cursors and occurrence_relays both reference nodes; they revert first.
DROP TABLE IF EXISTS nodes RESTRICT;
