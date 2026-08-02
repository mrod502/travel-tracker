-- ====================================================================
-- ENUM TYPES
-- Defined upfront so tables can reference them
-- ====================================================================

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
