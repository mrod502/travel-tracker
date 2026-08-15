-- Device identity queries (most common: trace a device's history)
CREATE INDEX idx_occurrence_device_hash    ON occurrences (device_hash, observed_at DESC);

-- Node activity queries
CREATE INDEX idx_occurrence_origin_node    ON occurrences (origin_node_id, observed_at DESC);
-- NOTE: reporting_node_id index moved to occurrence_relays table

-- H3 geo-indexing (fine and macro resolution)
CREATE INDEX idx_occurrence_geo_fine       ON occurrences (geo_cell_fine, observed_at DESC);
CREATE INDEX idx_occurrence_geo_macro      ON occurrences (geo_cell_macro, observed_at DESC);

-- PostGIS spatial queries (proximity searches)
CREATE INDEX idx_occurrence_location       ON occurrences USING GIST (location);

-- =====================================================================
-- Occurrence Relays Indexes
-- =====================================================================

-- Query relays by reporting node (who reported what)
CREATE INDEX idx_occurrence_relay_reporting_node 
    ON occurrence_relays (reporting_node_id, observed_at DESC);

-- Query relays by occurrence (quick lookup)
CREATE INDEX idx_occurrence_relay_occurrence 
    ON occurrence_relays (occurrence_id, observed_at);

-- Query relays by geo cell (find all relays in a region)
CREATE INDEX idx_occurrence_relay_geo_cell 
    ON occurrence_relays (geo_cell_macro, observed_at DESC);
