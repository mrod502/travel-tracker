-- =====================================================================
-- Distributed BLE Device Tracking - Initial Schema Migration
-- Creates core tables: nodes, occurrences (partitioned), and supporting indexes
-- =====================================================================


-- ---------------------------------------------------------------------
-- OCCURRENCES  (append-only, immutable ground truth)
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

    -- Node identification
    origin_node_id          TEXT NOT NULL REFERENCES nodes(node_id),
                                -- the node that CAPTURED and SIGNED this
                                -- observation (a signal node, if relayed)
    reporting_node_id       TEXT NOT NULL REFERENCES nodes(node_id),
                                -- the node that WROTE this row (equals
                                -- origin_node_id for full-node captures
                                -- the relaying aggregator otherwise)

    -- Timestamps
    observed_at             TIMESTAMPTZ NOT NULL,   -- sync-corrected UTC time
    observed_at_node_local  TIMESTAMPTZ NOT NULL,   -- raw, pre-correction, for drift auditing

    -- Device information
    device_address          TEXT,                    -- raw BLE MAC, nullable (may be withheld/hashed only)
    address_type            ble_address_type NOT NULL,
    advertised_name         TEXT,
    device_hash             TEXT NOT NULL,          -- pseudonymous id derived from address. indexed heavily

    -- Advertisement details
    adv_type                adv_type NOT NULL,
    rssi                    SMALLINT NOT NULL,
    tx_power                SMALLINT,
    service_uuids           JSONB,
    manufacturer_data       JSONB,
    raw_payload_hex         TEXT NOT NULL,

    -- Location data (PostGIS geography type)
    location                GEOGRAPHY(POINT, 4326), -- PostGIS point (lon, lat)
    alt_m                   REAL,
    accuracy_m              REAL,
    location_source         location_source NOT NULL,

    -- H3 geo-indexing (generated columns)
    -- Resolution 9 for fine-grained occurrence indexing (~0.1 km^2 cells)
    -- h3-pg v4+ accepts geometry/geography/point types
    geo_cell_fine           H3INDEX GENERATED ALWAYS AS
                                (h3_lat_lng_to_cell(ST_Force2D(location::geometry), 9)) STORED,
    -- Resolution 6 for macro-level node ownership (~36 km^2 cells)
    geo_cell_macro          H3INDEX GENERATED ALWAYS AS
                                (h3_cell_to_parent(h3_lat_lng_to_cell(ST_Force2D(location::geometry), 9), 6)) STORED,

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
