-- Revert 202607312137_create_bluetooth_occurrence_types.sql
--
-- Applied after the tables that use these enums have been dropped, so each
-- drop is expected to be clean. RESTRICT keeps it honest if they have not.
DROP TYPE IF EXISTS sync_direction RESTRICT;
DROP TYPE IF EXISTS adv_type RESTRICT;
DROP TYPE IF EXISTS location_source RESTRICT;
DROP TYPE IF EXISTS ble_address_type RESTRICT;
DROP TYPE IF EXISTS node_status RESTRICT;
DROP TYPE IF EXISTS node_type RESTRICT;
DROP TYPE IF EXISTS signal_type RESTRICT;
