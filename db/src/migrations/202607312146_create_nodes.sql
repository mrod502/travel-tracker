-- ---------------------------------------------------------------------
-- NODES REGISTRY
-- Local cache of known peers, synced via gossip membership (SWIM) and
-- issued via the CA at enrollment. Not authoritative on its own --
-- authority is the CA cert + gossip liveness, this is just a local view.
-- ---------------------------------------------------------------------

CREATE TABLE nodes (
    node_id                TEXT PRIMARY KEY,          -- e.g. 'node-042'
    node_type              node_type NOT NULL,

    -- Transport identity (mTLS) -- used for network connections.
    -- NULL for signal nodes, which never do a TLS handshake (LoRa has no TLS).
    mtls_cert_fingerprint  TEXT,

    -- Provenance identity (signing) -- used for occurrence-level signatures.
    -- EVERY node tier has one of these, including signal nodes, since every
    -- occurrence must be independently verifiable regardless of transport.
    signing_public_key     BYTEA NOT NULL,             -- raw Ed25519 public key (32 bytes)
    signing_key_algo       TEXT NOT NULL DEFAULT 'ed25519',

    -- CA enrollment credential: the CA's signature over (node_id || signing_public_key),
    -- issued once at registration. This is what lets ANY party verify a node's
    -- signing key traces back to the CA without a live CA call at verify-time --
    -- the credential is distributed once (via node sync) and cached locally.
    ca_credential          BYTEA NOT NULL,

    -- Fixed location (for stationary nodes like signal nodes or aggregators)
    fixed_lat              DOUBLE PRECISION,           -- null if mobile
    fixed_lon              DOUBLE PRECISION,

    -- Geo-ownership (for full nodes): res-6 H3 cells this node is authoritative for
    owns_geo_cells         H3INDEX[],

    -- Audit fields
    registered_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at           TIMESTAMPTZ,
    status                 node_status NOT NULL DEFAULT 'active'
);

CREATE INDEX idx_node_type ON nodes (node_type);
CREATE INDEX idx_node_owns_geo_cells ON nodes USING GIN (owns_geo_cells);
CREATE INDEX idx_node_status ON nodes (status);

COMMENT ON TABLE nodes IS
    'signing_public_key + ca_credential together let any party verify any
     occurrence''s origin offline: check ca_credential against the CA''s known
     root key once, then verify the occurrence signature against signing_public_key.
     Neither step requires a live CA call at verification time.';

