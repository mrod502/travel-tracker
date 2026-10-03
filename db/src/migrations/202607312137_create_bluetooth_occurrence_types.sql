--migrate:up.begin
-- ====================================================================
-- ENUM TYPES
-- Defined upfront so tables can reference them
-- ====================================================================

-- Signal type enum (supports multiple wireless technologies)
CREATE TYPE signal_type AS ENUM ('bluetooth', 'wifi', 'nfc', 'zigbee', 'lorawan');

-- Node type enum
CREATE TYPE node_type AS ENUM ('full', 'light', 'aggregator', 'signal');

-- Node status enum
CREATE TYPE node_status AS ENUM ('active', 'suspected', 'down', 'revoked');

-- BLE address type enum (Bluetooth SIG spec)
CREATE TYPE ble_address_type AS ENUM ('public', 'random_static', 'random_resolvable', 'random_nonresolvable');

-- Location source enum
CREATE TYPE location_source AS ENUM ('node_fixed', 'node_gps', 'interpolated', 'aggregator_fixed');

-- Advertisement type enum
CREATE TYPE adv_type AS ENUM ('connectable_adv', 'scannable_adv', 'broadcast_adv', 'extended_adv');

-- Sync direction enum
CREATE TYPE sync_direction AS ENUM ('inbound', 'outbound');
--migrate:up.end

--migrate:down.begin
-- Revert the statements above.
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
--migrate:down.end
