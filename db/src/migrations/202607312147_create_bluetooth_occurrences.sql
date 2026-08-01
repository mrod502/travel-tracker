-- Create table for storing Bluetooth device occurrence events
-- Flat structure optimized for append-only writes and time-based queries

CREATE TABLE bluetooth_occurrences (
    -- Primary key: time-sortable UUID (stored as text for UUIDv7 compatibility)
    occurrence_id TEXT PRIMARY KEY,

    -- Node identification
    node_id TEXT NOT NULL,

    -- Timestamps
    observed_at TIMESTAMPTZ NOT NULL,           -- UTC time after clock sync correction
    observed_at_node_local TIMESTAMPTZ,         -- Raw node timestamp before sync correction

    -- Device information (flattened)
    device_address TEXT NOT NULL,               -- BLE MAC address (may be randomized)
    device_address_type TEXT,                   -- public | random_static | random_resolvable | random_nonresolvable
    device_advertised_name TEXT,                -- Device name from AD payload (nullable)
    device_hash TEXT,                           -- Stable pseudonymous ID for privacy (nullable)

    -- Advertisement details (flattened)
    advertisement_type TEXT,                    -- ADV_IND, ADV_NONCONN_IND, SCAN_RSP, etc.
    rssi INTEGER,                               -- Signal strength (nullable)
    tx_power INTEGER,                           -- TX power from AD payload (nullable)
    service_uuids TEXT[],                       -- Advertised service UUIDs (PostgreSQL array)
    manufacturer_company_id TEXT,               -- Company ID from manufacturer data (nullable)
    manufacturer_payload_hex TEXT,              -- Raw manufacturer data payload (nullable)
    raw_payload_hex TEXT,                       -- Full raw AD payload for reprocessing (nullable)

    -- Location data (flattened)
    location_lat NUMERIC(10, 7),                -- Latitude (7 decimal places ≈ 1cm accuracy)
    location_lon NUMERIC(11, 7),                -- Longitude (7 decimal places ≈ 1cm accuracy)
    location_alt_m NUMERIC(8, 3),               -- Altitude in meters (3 decimal places = 1mm)
    location_accuracy_m NUMERIC(6, 3),          -- GPS fix accuracy (3 decimal places = 1mm)
    location_source TEXT,                       -- node_fixed | node_gps | interpolated

    -- Schema version for forward compatibility
    schema_version INTEGER NOT NULL DEFAULT 1,

    -- Audit timestamp
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes for common query patterns
CREATE INDEX idx_bluetooth_occurrences_device_address 
    ON bluetooth_occurrences(device_address);

CREATE INDEX idx_bluetooth_occurrences_observed_at 
    ON bluetooth_occurrences(observed_at DESC);

CREATE INDEX idx_bluetooth_occurrences_node_id 
    ON bluetooth_occurrences(node_id);

CREATE INDEX idx_bluetooth_occurrences_location 
    ON bluetooth_occurrences(location_lat, location_lon);

