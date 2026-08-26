-- ---------------------------------------------------------------------
-- NODE REVOCATIONS
-- Tracks revoked nodes and their revocation metadata.
-- This table is populated when the CA revokes a node's credentials.
-- ---------------------------------------------------------------------

CREATE TABLE node_revocations (
    node_id                BYTEA PRIMARY KEY,         -- SHA-256(signing_public_key), 32 bytes
    revoked_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_by             TEXT NOT NULL,              -- CA identifier that issued the revocation
    reason                 INT NOT NULL,               -- Revocation reason code (0-6 per RFC 5280)
    signing_public_key     BYTEA NOT NULL,             -- Ed25519 public key (32 bytes)
    ca_credential          BYTEA NOT NULL,             -- CA credential at time of revocation (for audit)
    rsl_sequence_number    BIGINT NOT NULL,            -- RSL sequence number where this was first revoked
    notes                  TEXT,                       -- Optional audit notes

    -- Constraint: reason must be in valid range (0-6)
    CONSTRAINT valid_revocation_reason CHECK (reason >= 0 AND reason <= 6)
);

CREATE INDEX idx_node_revocations_revoked_at ON node_revocations (revoked_at);
CREATE INDEX idx_node_revocations_reason ON node_revocations (reason);
CREATE INDEX idx_node_revocations_ca ON node_revocations (revoked_by);

COMMENT ON TABLE node_revocations IS $$
Revocation tracking for node credentials.

When a node's CA credential is revoked, an entry is inserted here with:
- node_id: The 32-byte SHA-256 hash of the signing public key
- revoked_at: When the revocation occurred
- revoked_by: The CA identifier that issued the revocation
- reason: Revocation reason code (0=unspecified, 1=key_compromise, 
          2=ca_compromise, 3=ceased_operation, 4=policy_violation,
          5=superseded, 6=hold)
- signing_public_key: The node's Ed25519 public key (for verification)
- ca_credential: The CA credential that was revoked (for audit trail)
- rsl_sequence_number: The RSL sequence number where this was first recorded
- notes: Optional audit notes

This table is used to:
1. Generate Revocation Status Lists (RSLs)
2. Check if a node should be accepted for data recording
3. Check if a node should be allowed to connect (handshake)
4. Maintain an audit trail of all revocations

Note: A revoked node's entry in the nodes table should have status='revoked'
to reflect the revocation in the local cache.
$$;

COMMENT ON COLUMN node_revocations.reason IS $$
Revocation reason codes per RFC 5280 CRL Entry Reason Codes:
0 - unspecified
1 - keyCompromise (private key suspected compromised)
2 - cACompromise (CA compromise detected)
3 - affiliationChanged (node decommissioned/replaced)
4 - superseded (new credential issued, old one revoked)
5 - cessationOfOperation (node ceased operation)
6 - certificateHold (temporary hold pending investigation)

Note: We use a simplified mapping:
- Unspecified = 0
- KeyCompromise = 1
- CaCompromise = 2
- CeasedOperation = 3 (maps to affiliationChanged/cessationOfOperation)
- PolicyViolation = 4 (not in RFC 5280, but useful for BTMon)
- Superseded = 5
- Hold = 6
$$;
