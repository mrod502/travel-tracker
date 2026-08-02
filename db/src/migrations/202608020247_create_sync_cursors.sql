-- ---------------------------------------------------------------------
-- SYNC BOOKKEEPING
-- Tracks per-peer replication cursor for application-level batch sync
-- over MQTT (since native logical replication assumes stable links,
-- and this network has intermittent nodes).
-- ---------------------------------------------------------------------

CREATE TABLE sync_cursors (
    peer_node_id            TEXT NOT NULL REFERENCES nodes(node_id),
    last_synced_occurrence_id UUID,
    last_synced_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    direction               sync_direction NOT NULL,

    PRIMARY KEY (peer_node_id, direction)
);

COMMENT ON TABLE sync_cursors IS
    'Application-level sync progress tracking for store-and-forward
     replication over intermittent links. Used for both inbound (receiving
     from peers) and outbound (sending to peers) cursors.';
