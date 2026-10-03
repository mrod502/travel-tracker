--migrate:up.begin
-- ====================================================================
-- DEVICE IDENTITY AND ASSOCIATION TABLES
--
-- Everything in this file is DERIVED. No capture path writes these rows and
-- nothing outside them depends on their contents, so a full reprocess is
-- "truncate and run the batch again" rather than a migration.
-- ====================================================================

-- How an address ended up belonging to an identity, ordered from strongest to
-- weakest evidence. The order is the one the identity design settled on:
-- fingerprint > temporal adjacency > name > IRK, with an exact address match in
-- front of all of them because it identifies the device rather than inferring it.
CREATE TYPE identity_resolution_method AS ENUM (
    'exact_address',
    'fingerprint',
    'temporal_adjacency',
    'name',
    'irk'
);

-- ---------------------------------------------------------------------
-- DEVICE IDENTITIES
-- One row per physical device as the resolver currently infers it. The
-- natural key is the fingerprint, not a serial: the batch is re-runnable,
-- and a re-run that invents a second identity for a device it already
-- knows would make every link it writes since then meaningless.
-- ---------------------------------------------------------------------

CREATE TABLE device_identities (
    identity_id UUID PRIMARY KEY DEFAULT uuidv7(),

    -- manufacturer id, service-UUID set hash, AD structure hash, name pattern and
    -- the kind of identifier the device presents. See the column comment below for
    -- the exact keys, which the resolver and this table have to agree on.
    fingerprint JSONB NOT NULL,

    -- SHA-256 over the canonical (key-sorted) fingerprint. This is what a re-run
    -- looks a device up by, so it is unique across the whole table rather than
    -- per observer: two nodes that both saw enough of the same device to derive the
    -- same fingerprint are describing one device, which is the entire point.
    fingerprint_hash BYTEA NOT NULL,

    -- bt_iden's confidence in this identity, 0..1.
    confidence_score REAL NOT NULL,

    -- How the identity was established in the first place.
    resolution_method identity_resolution_method NOT NULL,

    first_seen TIMESTAMPTZ NOT NULL, -- earliest occurrence folded in
    last_seen TIMESTAMPTZ NOT NULL, -- latest occurrence folded in
    observation_count INT NOT NULL,

    -- The resolver build that wrote this row, so a decision in a report can be
    -- traced back to the scoring model that produced it.
    resolver_version TEXT NOT NULL,
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- The fingerprint is a fixed shape, not an arbitrary bag: an adapter that
    -- forgets a key and stores {} would otherwise merge every such device into one
    -- identity with an empty fingerprint.
    CONSTRAINT valid_fingerprint_shape CHECK (
        jsonb_typeof(fingerprint) = 'object'
        AND fingerprint ? 'manufacturer_id'
        AND fingerprint ? 'service_uuids_hash'
        AND fingerprint ? 'field_layout_hash'
        AND fingerprint ? 'name_pattern'
    ),

    CONSTRAINT valid_identity_confidence CHECK (
        confidence_score >= 0.0 AND confidence_score <= 1.0
    ),

    -- An identity is a record of having seen something at least once.
    CONSTRAINT valid_identity_observation_count CHECK (observation_count > 0),

    CONSTRAINT valid_identity_seen_window CHECK (last_seen >= first_seen),

    UNIQUE (fingerprint_hash)
);

-- The device behind a stored occurrence: hash it and look the identity up.
CREATE INDEX idx_device_identities_fingerprint_hash ON device_identities (fingerprint_hash);

-- A review pass over the identities a human should look at, weakest first, and
-- "what has this node's resolver concluded lately".
CREATE INDEX idx_device_identities_confidence ON device_identities (confidence_score);
CREATE INDEX idx_device_identities_last_seen ON device_identities (last_seen DESC);

-- Which identities advertise a given manufacturer id or service set. The
-- fingerprint is the only place that information lives.
CREATE INDEX idx_device_identities_fingerprint ON device_identities USING GIN (fingerprint);

COMMENT ON TABLE device_identities IS $$
Stable device identities inferred from occurrences by the bt_iden resolver.

Fully reprocessable: this table describes what the batch currently believes, and
a re-run over the same occurrences is expected to reproduce it. Nothing in the
capture path writes here, and no other table depends on these rows.

Identity is keyed by fingerprint (manufacturer id + service-UUID set + AD
structure + name pattern), not by address, because a device that rotates its
resolvable private addresses is one device advertising from many. The address
itself is what device_address_links maps in.

An identity here is a best-effort inference, not a proven fact. confidence_score
and resolution_method exist so a consumer can decline the weak ones.
$$;

COMMENT ON COLUMN device_identities.fingerprint IS $$
The features the identity was built from, as an object with these keys:

  manufacturer_id      - the Bluetooth SIG company id, or null when never seen
  service_uuids_hash   - hex SHA-256 over the sorted advertised service UUID set,
                         or null when the list was never seen
  field_layout_hash    - hex SHA-256 over the AD type-code sequence read off the
                         advertisement, or null when structures were not captured
  name_pattern         - the advertised local name with a trailing digit run
                         replaced by '*', or null when no name was advertised
  identifier_source    - "ble_mac", "uuid" or "opaque_id", matching
                         signal_payload.ble.id_source

Nulls are per-feature on purpose: a backend that cannot see AD structures leaves
field_layout_hash null rather than inventing one, and two devices observed by
different backends then cannot collide on a hash neither of them computed.
$$;

COMMENT ON COLUMN device_identities.fingerprint_hash IS $$
SHA-256 over the fingerprint serialized with sorted keys and no insignificant
whitespace - the same bytes for the same features regardless of which adapter
built them. Unique across the table, so the batch converges on one identity per
distinct fingerprint instead of one per run.
$$;

-- ---------------------------------------------------------------------
-- DEVICE ADDRESS LINKS
-- The rotating identifier -> identity mapping, with the window it was
-- observed over and the heuristic that justified it. Kept separate from
-- device_identities because the mapping is the volatile half: addresses
-- change, the identity does not.
-- ---------------------------------------------------------------------

CREATE TABLE device_address_links (
    -- The rotating identifier as occurrences store it: SHA-256 over the 6-byte
    -- MAC, or over a tagged platform identifier (app/src/node/device_id.rs).
    device_hash BYTEA NOT NULL,

    -- The node whose occurrences produced this link. Resolution is per-observer:
    -- a node can only reason about the devices inside its own radio horizon, and
    -- a platform-assigned identifier is comparable only between occurrences that
    -- same node captured.
    observer_node_id BYTEA NOT NULL REFERENCES nodes(node_id),

    identity_id UUID NOT NULL REFERENCES device_identities(identity_id),

    -- The MAC itself, when the identifier was a MAC. Null for a platform UUID or
    -- opaque identifier, which has no address behind it.
    address BYTEA,

    address_type ble_address_type, -- which kind of BLE address, when it was one

    method identity_resolution_method NOT NULL,
    confidence REAL NOT NULL,

    first_seen TIMESTAMPTZ NOT NULL,
    last_seen TIMESTAMPTZ NOT NULL,
    observation_count INT NOT NULL,

    -- A reprocess needs to say which pass wrote the row, so a report can be
    -- reconciled against the batch that produced it.
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- The link is per (identifier, observer, identity). A device_hash held by two
    -- identities from the same observer is a SPLIT, which is a real outcome the
    -- resolver has to be able to record rather than a constraint violation to
    -- paper over - and the identifier stays first in the key because "which
    -- identity is this device" is the query that runs on every occurrence.
    PRIMARY KEY (device_hash, observer_node_id, identity_id),

    -- 32 bytes is SHA-256, which is what derive_device_identity produces. A link
    -- written with a raw 6-byte MAC here would silently never match an occurrence.
    CONSTRAINT valid_link_device_hash CHECK (octet_length(device_hash) = 32),

    -- A MAC is 6 bytes when it is one at all.
    CONSTRAINT valid_link_address CHECK (address IS NULL OR octet_length(address) = 6),

    CONSTRAINT valid_link_confidence CHECK (confidence >= 0.0 AND confidence <= 1.0),

    CONSTRAINT valid_link_observation_count CHECK (observation_count > 0),

    CONSTRAINT valid_link_seen_window CHECK (last_seen >= first_seen)
);

-- "What have we concluded about this identifier" for every occurrence read.
CREATE INDEX idx_device_address_links_hash ON device_address_links (device_hash);

-- Rebuilding one identity's history, and finding the links a given node made.
CREATE INDEX idx_device_address_links_identity ON device_address_links (identity_id);
CREATE INDEX idx_device_address_links_observer_window
    ON device_address_links (observer_node_id, last_seen DESC);

COMMENT ON TABLE device_address_links IS $$
Which rotating device identifiers the resolver currently believes belong to
which identity, per observing node, over what window, and by which heuristic.

device_hash is the identifier as occurrences store it, so this table is the join
between an occurrence row and an inferred identity. method records how the link
was justified - exact_address identifies a device, fingerprint and
temporal_adjacency and name infer one - so a consumer can tell the two apart.

An identifier linked to two identities by the same observer is a recorded split,
not a contradiction to be resolved here: it is what the batch saw.
$$;

COMMENT ON COLUMN device_address_links.observer_node_id IS $$
The node whose occurrences justify the link. Cross-node linking is only sound for
MAC-derived identifiers, which are globally comparable; a platform-assigned
identifier hashes differently on every host, so its links are scoped to the node
that saw it.
$$;

-- ---------------------------------------------------------------------
-- CO-OCCURRENCE EVENTS
-- The raw, windowed join: two identities seen together at one node in one
-- cell over one window. Canonical identity ordering keeps (a,b) and (b,a)
-- from being two facts.
-- ---------------------------------------------------------------------

CREATE TABLE co_occurrence_events (
    identity_a UUID NOT NULL REFERENCES device_identities(identity_id),
    identity_b UUID NOT NULL REFERENCES device_identities(identity_id),

    -- The node that saw both. Two devices seen by different nodes are not known
    -- to be together, however close the cells.
    node_id BYTEA NOT NULL REFERENCES nodes(node_id),

    -- The cell their overlap falls in. Resolution 6 (the macro cell), because
    -- "in the same place" at resolution 9 is a much stronger claim than the
    -- location accuracy supports.
    geo_cell_macro H3INDEX NOT NULL,

    window_start TIMESTAMPTZ NOT NULL,
    window_end TIMESTAMPTZ NOT NULL,

    -- Paired observations inside the window: one is a coincidence, many is a
    -- co-presence with a duration.
    sample_count INT NOT NULL,

    -- Distance between the two devices' locations at the overlap, when both were
    -- located and the node reported enough to compute it.
    distance_m REAL,

    generated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    PRIMARY KEY (identity_a, identity_b, node_id, geo_cell_macro, window_start),

    -- One row per unordered pair: without this, every aggregate downstream either
    -- double-counts or has to canonicalise on the fly, in every query.
    CONSTRAINT canonical_co_occurrence_order CHECK (identity_a < identity_b),

    CONSTRAINT distinct_identities CHECK (identity_a <> identity_b),

    CONSTRAINT valid_co_occurrence_window CHECK (window_end >= window_start),

    CONSTRAINT valid_co_occurrence_samples CHECK (sample_count > 0),

    CONSTRAINT valid_co_occurrence_distance CHECK (distance_m IS NULL OR distance_m >= 0.0)
);

-- "What was seen together during this period", the replay and backfill scan.
CREATE INDEX idx_co_occurrence_window_start ON co_occurrence_events (window_start DESC);

-- The reverse direction. The primary key leads with identity_a, so without this
-- an ask about the second identity in the pair is a sequential scan.
CREATE INDEX idx_co_occurrence_identity_b ON co_occurrence_events (identity_b, window_start DESC);

COMMENT ON TABLE co_occurrence_events IS $$
Raw output of the windowed self-join on occurrences: two device identities seen
together by one node, in one macro cell, over one window.

Append-style and reprocessable. association_edges is the queryable rollup of
these rows; this table is what a recompute reads, and what makes a strength score
auditable back to the sightings behind it.

identity_a is always the lower identity_id: the pair is unordered, and without a
canonical order each co-presence would be stored twice.
$$;

-- ---------------------------------------------------------------------
-- ASSOCIATION EDGES
-- The queryable relationship: how strongly two identities appear to travel
-- together, with the three numbers the composite score was made from kept
-- beside it so the score can be argued with.
-- ---------------------------------------------------------------------

CREATE TABLE association_edges (
    identity_a UUID NOT NULL REFERENCES device_identities(identity_id),
    identity_b UUID NOT NULL REFERENCES device_identities(identity_id),

    co_occurrence_count INT NOT NULL,
    distinct_geo_cells INT NOT NULL,
    distinct_days INT NOT NULL,

    first_seen TIMESTAMPTZ NOT NULL,
    last_seen TIMESTAMPTZ NOT NULL,

    -- Composite of the three counts above, weighting geo and day diversity over
    -- raw count: two devices that share a busy transit stop every day are not
    -- associated, two that turn up together in three places on five different
    -- days are.
    association_strength REAL NOT NULL,

    -- Events through which this aggregate runs. Without it, a reader cannot tell
    -- a stale edge from one the batch simply has not reached yet.
    computed_through TIMESTAMPTZ NOT NULL,
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    PRIMARY KEY (identity_a, identity_b),

    CONSTRAINT canonical_edge_order CHECK (identity_a < identity_b),

    CONSTRAINT distinct_edge_identities CHECK (identity_a <> identity_b),

    CONSTRAINT valid_edge_strength CHECK (
        association_strength >= 0.0 AND association_strength <= 1.0
    ),

    CONSTRAINT valid_edge_aggregates CHECK (
        co_occurrence_count >= 0
        AND distinct_geo_cells >= 0
        AND distinct_days >= 0
    ),

    CONSTRAINT valid_edge_seen_window CHECK (last_seen >= first_seen)
);

-- The question this table exists for: strongest relationships first.
CREATE INDEX idx_association_edges_strength ON association_edges (association_strength DESC);

-- The other end of the edge. The primary key leads with identity_a.
CREATE INDEX idx_association_edges_identity_b ON association_edges (identity_b);

COMMENT ON TABLE association_edges IS $$
Aggregated co-presence between two device identities, computed from
co_occurrence_events.

co_occurrence_count, distinct_geo_cells and distinct_days are stored alongside
association_strength rather than folded away into it: the composite score is a
judgement, and a reviewer needs the inputs to disagree with it.

The score weights diversity over volume on purpose. Raw count alone ranks two
devices parked at the same transit stop above two that travel together.
$$;
--migrate:up.end

--migrate:down.begin
-- Revert the statements above.
--
-- Every object here is derived, so the revert loses no primary data: the
-- occurrences the batch read are untouched, and re-running the batch against
-- them rebuilds all four tables. What is lost is the resolver's accumulated
-- judgement - identities, which address belonged to which identity, and any
-- association anyone has acted on - plus the history that a strength score was
-- computed from, which cannot be recovered once the events are gone.
--
-- Reverse dependency order, and RESTRICT throughout: association_edges and
-- co_occurrence_events both point at device_identities, so if anything unexpected
-- still references these tables the revert fails instead of quietly taking it with
-- it.
DROP TABLE IF EXISTS association_edges RESTRICT;
DROP TABLE IF EXISTS co_occurrence_events RESTRICT;
DROP TABLE IF EXISTS device_address_links RESTRICT;
DROP TABLE IF EXISTS device_identities RESTRICT;
DROP TYPE IF EXISTS identity_resolution_method RESTRICT;
--migrate:down.end
