-- Device identity queries (most common: trace a device's history)
CREATE INDEX idx_occurrence_device_hash    ON occurrences (device_hash, observed_at DESC);

-- Node activity queries
CREATE INDEX idx_occurrence_origin_node    ON occurrences (origin_node_id, observed_at DESC);
CREATE INDEX idx_occurrence_reporting_node ON occurrences (reporting_node_id, observed_at DESC);

-- H3 geo-indexing (fine and macro resolution)
CREATE INDEX idx_occurrence_geo_fine       ON occurrences (geo_cell_fine, observed_at DESC);
CREATE INDEX idx_occurrence_geo_macro      ON occurrences (geo_cell_macro, observed_at DESC);

-- PostGIS spatial queries (proximity searches)
CREATE INDEX idx_occurrence_location       ON occurrences USING GIST (location);
