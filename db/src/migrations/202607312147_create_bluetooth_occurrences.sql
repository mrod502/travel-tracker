-- =====================================================================
-- Distributed Wireless Signal Tracking - Initial Schema Migration
-- Creates core tables: nodes, occurrences (partitioned), and supporting indexes
-- Supports: Bluetooth, WiFi, and future signal types (NFC, Zigbee, etc.)
-- =====================================================================


-- ---------------------------------------------------------------------
-- OCCURRENCES  (append-only, immutable ground truth)
-- Single unified table for all wireless signal types.
-- Partitioned by time (RANGE on observed_at) for retention/compaction.
-- Geo-sharding across nodes is enforced at the APPLICATION layer
-- (a node only ingests/syncs occurrences whose geo_cell it owns) --
-- true 2D partitioning isn't native to Postgres, so geo_cell is an
-- indexed column here, not a partition key.
-- ---------------------------------------------------------------------

CREATE TABLE occurrences (
    -- Primary key: UUIDv7 (time-sortable) for full-node-originated observations,
    -- or deterministic hash for aggregator-relayed reports
    occurrence_id           UUID NOT NULL DEFAULT uuidv7(),

    -- Signal type discriminator (allows unified table for Bluetooth, WiFi, etc.)
    signal_type             signal_type NOT NULL,     -- bluetooth, wifi, nfc, zigbee, etc.

    -- Node identification (origin only; relay tracking moved to occurrence_relays table)
    origin_node_id          BYTEA NOT NULL REFERENCES nodes(node_id),
                                -- the node that CAPTURED and SIGNED this
                                -- observation (a signal node, if relayed)
    -- NOTE: reporting_node_id was moved to occurrence_relays table
    -- to separate occurrence data from relay provenance concerns.

    -- Timestamps
    observed_at             TIMESTAMPTZ NOT NULL,   -- sync-corrected UTC time
    observed_at_node_local  TIMESTAMPTZ NOT NULL,   -- raw, pre-correction, for drift auditing

    -- Device information (common across signal types)
    device_address          BYTEA,                   -- raw MAC/address (binary, nullable)
    device_hash             BYTEA NOT NULL,          -- pseudonymous id (32-byte SHA-256 hash)
    advertised_name         TEXT,

    -- Advertisement details (BLE-specific; null for other signal types)
    adv_type                adv_type,                -- BLE only
    rssi                    SMALLINT NOT NULL,
    tx_power                SMALLINT,

    -- Flexible signal-specific payload (JSONB for extensibility)
    -- Bluetooth: service_uuids, manufacturer_data, raw_payload_hex
    -- WiFi: ssid, bssid, channel, capabilities, etc.
    signal_payload          JSONB NOT NULL DEFAULT '{}',

    -- Location data (PostGIS geography type)
    location                GEOGRAPHY(POINT, 4326), -- PostGIS point (lon, lat)
    alt_m                   REAL,
    accuracy_m              REAL,
    location_source         location_source NOT NULL,

    -- H3 geo-indexing (generated columns)
    -- Resolution 9 for fine-grained occurrence indexing (~0.1 km^2 cells)
    -- h3-pg v4+ expects point(lat, lng) - note: lat first, then lng
    -- location is GEOGRAPHY(POINT, 4326), so we extract Y (lat) and X (lng)
    geo_cell_fine           H3INDEX GENERATED ALWAYS AS
                                (h3_latlng_to_cell(point(ST_Y(location::geometry), ST_X(location::geometry)), 9)) STORED,
    -- Resolution 6 for macro-level node ownership (~36 km^2 cells)
    geo_cell_macro          H3INDEX GENERATED ALWAYS AS
                                (h3_cell_to_parent(h3_latlng_to_cell(point(ST_Y(location::geometry), ST_X(location::geometry)), 9), 6)) STORED,

    -- PROVENANCE: every occurrence is independently verifiable, regardless
    -- of who relayed it. signed_payload is the canonical byte sequence the
    -- origin node actually signed (see docs/provenance.md for exact field
    -- order/encoding) -- stored explicitly rather than reconstructed, so a
    -- verifier never has to guess at canonicalization rules that may drift
    -- across schema_version changes.
    signed_payload          BYTEA NOT NULL,
    signature               BYTEA NOT NULL,         -- Ed25519 sig over signed_payload,
                                  -- verify against node(origin_node_id).signing_public_key

    -- Schema version for forward compatibility
    schema_version          SMALLINT NOT NULL DEFAULT 1,

    -- Audit timestamp: when THIS node wrote the row
    ingested_at             TIMESTAMPTZ NOT NULL DEFAULT now(),

    PRIMARY KEY (occurrence_id, observed_at)   -- observed_at included: required by Postgres
                                                 -- for PK on a partitioned table
) PARTITION BY RANGE (observed_at);

-- ---------------------------------------------------------------------
-- OCCURRENCE_RELAYS - Relay provenance tracking (MVP deferred)
-- Separated from occurrences to distinguish:
-- 1. What was observed (occurrences - the core data)
-- 2. Who reported it on behalf of whom (occurrence_relays - relay metadata)
--
-- This table tracks WHEN an occurrence was relayed by which node,
-- allowing attribution of relay behavior without polluting the core
-- occurrence record. Critical for multi-hop relay scenarios in Phase 4+,
-- not required for MVP (single-hop aggregator relays).
-- ---------------------------------------------------------------------

CREATE TABLE occurrence_relays (
    occurrence_id    UUID NOT NULL,                     -- Must match occurrence_id in occurrences
    observed_at      TIMESTAMPTZ NOT NULL,              -- Must match occurrence timestamp (part of PK)
    geo_cell_macro   H3INDEX NOT NULL,                  -- Must match occurrence geo_cell_macro (part of PK)
    reporting_node_id BYTEA NOT NULL REFERENCES nodes(node_id),
    ingested_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (occurrence_id, observed_at, geo_cell_macro, reporting_node_id),
    -- Foreign key to occurrences must include all PK columns of the partitioned table
    CONSTRAINT fk_occurrence_relays_occurrence 
        FOREIGN KEY (occurrence_id, observed_at) 
        REFERENCES occurrences(occurrence_id, observed_at)
);

COMMENT ON TABLE occurrence_relays IS
    'Tracks which node(s) relayed an occurrence on behalf of the origin node.
     Used when a full node reports an occurrence from a signal node it aggregates.
     The core occurrences table stores origin_node_id (who captured/signed);
     this table stores reporting_node_id (who wrote it to this database).
     MVP (Phase 0-3) does not require this table; critical for Phase 4+ (signal nodes).
     See: .knowledge/architecture/data-model.md for occurrence vs relay separation.';

-- Example monthly partitions -- automate creation via cron/pg_partman in practice
CREATE TABLE occurrences_2026_07 PARTITION OF occurrences
    FOR VALUES FROM ('2026-07-01') TO ('2026-08-01');
CREATE TABLE occurrences_2026_08 PARTITION OF occurrences
    FOR VALUES FROM ('2026-08-01') TO ('2026-09-01');



COMMENT ON TABLE occurrences IS
    'Append-only, immutable. Never UPDATE. Dedup on sync via
     INSERT ... ON CONFLICT (occurrence_id, observed_at) DO NOTHING.
     Every row is independently verifiable: recompute/compare signed_payload
     and check signature against node(origin_node_id).signing_public_key.
     See docs/provenance.md for the exact verification procedure.';
