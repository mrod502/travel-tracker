-- ---------------------------------------------------------------------
-- REVOCATION STATUS LISTS
-- One row per Revocation Status List a CA has published, in the exact
-- shape it was signed and distributed.
--
-- The sequence number is the anti-replay mechanism: a node that has seen
-- RSL n for a CA must reject anything for that CA numbered n or lower.
-- That only holds if the number comes from state that outlives the
-- process that issued it, which is what this table is. Before it, the
-- only candidate was MAX(node_revocations.rsl_sequence_number) — a
-- column written when a revocation is *recorded*, so a CA with nothing
-- to revoke could never advance past its first list, and one captured
-- empty list was replayable forever.
-- ---------------------------------------------------------------------

CREATE TABLE revocation_status_lists (
    issuer_id        TEXT        NOT NULL,           -- CA identifier (SHA-256 of its public key, hex)
    sequence_number  BIGINT      NOT NULL,           -- Strictly increasing per issuer
    issued_at        TIMESTAMPTZ NOT NULL,           -- When the CA signed it
    expires_at       TIMESTAMPTZ NOT NULL,           -- Stale for policy after this
    revocation_count INT         NOT NULL,           -- Entries at publication time
    signature        BYTEA       NOT NULL,           -- Ed25519 signature over the RSL payload
    rsl              JSONB       NOT NULL,           -- The signed list, as distributed
    stored_at        TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- One list per (CA, sequence). This is the constraint that makes a
    -- replayed or duplicated sequence number a rejected write rather than
    -- a second row that nobody can tell apart from the first.
    PRIMARY KEY (issuer_id, sequence_number),

    -- 0 is reserved for "no RSL has been published yet"; a published list
    -- always carries a real sequence.
    CONSTRAINT valid_rsl_sequence CHECK (sequence_number > 0),

    -- An Ed25519 signature is always 64 bytes, so an unsigned list cannot
    -- be stored as if it were one.
    CONSTRAINT valid_rsl_signature CHECK (octet_length(signature) = 64),

    CONSTRAINT valid_rsl_validity CHECK (expires_at > issued_at),

    -- The header cannot disagree with the list it claims to summarise.
    CONSTRAINT valid_rsl_count CHECK (
        revocation_count = jsonb_array_length(rsl -> 'revocations')
    )
);

-- "Latest list for this CA", which is what every consumer asks for.
CREATE INDEX idx_rsl_issuer_issued_at ON revocation_status_lists (issuer_id, issued_at DESC);

COMMENT ON TABLE revocation_status_lists IS $$
Published Revocation Status Lists, one row per (issuer_id, sequence_number).

A CA generates a list from node_revocations, signs it, and stores it here;
the primary key is what makes the sequence number mean something, because
a second list carrying an already-used number is a rejected write rather
than an indistinguishable duplicate.

next sequence = MAX(sequence_number) for the issuer + 1, so the counter
survives the CA process restarting. node_revocations.rsl_sequence_number
keeps a different fact — the list in which each revocation first appeared
— and is not the source of the counter.

rsl holds the signed list exactly as distributed, so get_latest_rsl can
hand back the artifact that was signed rather than a fresh list rebuilt
from whatever the revocation table says now.
$$;

COMMENT ON COLUMN revocation_status_lists.sequence_number IS $$
Per-issuer anti-replay counter, starting at 1. 0 means "nothing published
yet" and is what an empty table reads as; the CHECK rejects storing it.
$$;

COMMENT ON COLUMN revocation_status_lists.rsl IS $$
The serialized signed list, including its revocation entries and signature.
revocation_count is checked against this document's own array length so the
indexed header cannot drift from the payload it summarises.
$$;
