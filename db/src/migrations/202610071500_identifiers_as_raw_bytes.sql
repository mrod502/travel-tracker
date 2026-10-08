--migrate:up.begin
-- ---------------------------------------------------------------------
-- CA IDENTIFIERS AS RAW BYTES
--
-- Every other identifier in this schema is already BYTEA: nodes.node_id,
-- occurrences.origin_node_id, node_revocations.node_id, signing_public_key,
-- ca_credential, signature. The two CA-side columns below were the exception,
-- holding the same 32-byte SHA-256 digest as 64 hex characters in TEXT.
--
-- That made a CA id the only identifier a consumer had to decode before using it
-- and a writer had to encode before storing, with the format agreed in prose
-- rather than by the type. It also made joins between a CA id and a node id
-- impossible: the same kind of value in two types.
--
-- Keys, signatures and identifiers are raw bytes in the database and in the
-- program; an encoding appears only in a file format that needs one (PEM for a
-- key, base64 for a byte field inside a JSON document) or where a person reads it.
--
-- The `rsl` documents change spelling too, and that is noted below rather than
-- rewritten, because it is a signed artifact.
-- ---------------------------------------------------------------------

-- The CA identifier, from hex text to the 32 bytes it has always been.
ALTER TABLE revocation_status_lists
    ALTER COLUMN issuer_id TYPE BYTEA USING decode(issuer_id, 'hex');

-- The same check every other identifier column gets from its type: an id is a
-- SHA-256 digest, so anything else is a mistake at the point of writing rather
-- than a puzzle for the next reader.
ALTER TABLE revocation_status_lists
    ADD CONSTRAINT valid_rsl_issuer CHECK (octet_length(issuer_id) = 32);

-- `revoked_by` carries a second fact as well as a CA id: the string 'pending'
-- meant "recorded, but no signed list has attributed it to a CA yet". With the
-- column holding 32 bytes, that state has an honest representation — NULL — and
-- no invented value that every reader has to know to exclude.
ALTER TABLE node_revocations
    ALTER COLUMN revoked_by DROP NOT NULL;

ALTER TABLE node_revocations
    ALTER COLUMN revoked_by TYPE BYTEA
        USING CASE WHEN revoked_by = 'pending' THEN NULL ELSE decode(revoked_by, 'hex') END;

COMMENT ON COLUMN revocation_status_lists.issuer_id IS $$
SHA-256 of the CA's signing public key, 32 raw bytes — the same value
ca::TrustAnchor::ca_id() returns, which is the key a consumer verifies
`signature` against.
$$;

COMMENT ON COLUMN node_revocations.revoked_by IS $$
SHA-256 of the revoking CA's public key, 32 raw bytes. NULL means the
revocation is recorded but unpublished: no signed list has attributed it to a
CA yet, and store_rsl fills this in when one does.
$$;

-- The stored document is signed material, so this migration does not touch it.
-- It is worth stating what that means, because the column comment above the
-- change described a document that no longer exists in that form.
COMMENT ON COLUMN revocation_status_lists.rsl IS $$
The signed list as distributed: keys, signatures and identifiers base64, the
same fields ca::RevocationStatusList carries.

A list published before 2026-10-07 is a different document in two ways — its
byte fields were JSON arrays of integers, and its signature covered the issuer
id as 64 hex characters rather than as 32 bytes — and such a row does not read
back into the current type. Republish it (ca ca-generate-rsl) rather than
editing the row: the signature is the whole reason the row is trusted, and a
document whose signature cannot be checked is not the CA's.
$$;
--migrate:up.end

--migrate:down.begin
-- Revert the statements above.
--
-- Back to hex text, with unpublished revocations named 'pending' again because
-- the column cannot be NULL in the reverted schema. Nothing else in the row
-- changes: this is a representation revert, not a data change — except that a
-- list stored under the current format still will not read back into whatever
-- code the revert is meant to restore.
ALTER TABLE node_revocations
    ALTER COLUMN revoked_by TYPE TEXT
        USING COALESCE(encode(revoked_by, 'hex'), 'pending');

ALTER TABLE node_revocations
    ALTER COLUMN revoked_by SET NOT NULL;

ALTER TABLE revocation_status_lists
    DROP CONSTRAINT valid_rsl_issuer;

ALTER TABLE revocation_status_lists
    ALTER COLUMN issuer_id TYPE TEXT USING encode(issuer_id, 'hex');
--migrate:down.end
