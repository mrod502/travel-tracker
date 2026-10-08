# Provenance Feature Implementation Status

**Analysis Date:** 2026-09-02 (originally 2026-08-26)
**Updated:** 2026-09-18 — payload **v2** is now the canonical CBOR spec's **positional array**
([GAP_ANALYSIS B8](../../GAP_ANALYSIS.md#81-blocking)/[B9](../../GAP_ANALYSIS.md#81-blocking)): eighteen
elements, byte strings for binary fields, code tables for the two enum fields, a checked-in golden
vector, and a decoder that refuses arity, shape, code and trailing-byte violations. §1 and the
"Inconsistencies" §1/§4 describe it; the spec was amended rather than the implementation left alone, so
the row coverage B7 added survived the change.
**Updated:** 2026-09-16 — payload **v2** ([GAP_ANALYSIS B7](../../GAP_ANALYSIS.md#81-blocking)): the
signature now covers the whole row the node authors, and the doc's §1 and §5 describe the two-version
encoder. Everything else below is as written, including the parts that did not change: nothing on the
read path consumes a signature.
**Updated:** 2026-10-08 — revocation is wired ([GAP_ANALYSIS B11](../../GAP_ANALYSIS.md#81-blocking),
closing M11/M12/M17). The node-side read path now consumes one signature: `ca::verified_checker`
verifies the CA's list against the anchor in `[revocation].anchor_path` and the node refuses to record
what that list revokes or can no longer vouch for. Where this document says the checker is empty,
`enable_revocation_checking` only logs, or `verify_occurrence_with_revocation` is the composed
workflow, it is describing the state before that date — `enable_revocation_checking` and
`app/src/provenance/verify_with_revocation.rs` are both gone, replaced by
`app/src/node/revocation.rs`. §3 and §6's remaining findings (credential verification, `nodes.status`
drift, no occurrence-verification call site) still stand.
**Status:** Partially Implemented — crypto complete and consumed for revocation; CA enrollment operator-driven, credential verification not wired into the node

---

## Executive Summary

The provenance feature is **~70% complete**, but the shape of the remaining 30% has changed
since the August analysis. The cryptographic primitives are fully implemented and tested. CA
enrollment is **no longer missing** — `ca-init`, `ca-enroll`, `ca-verify`, `ca-revoke` and
`ca-generate-rsl` exist and exercise the `ca` crate's credential issuance and verification
code. Revocation is **wired since 2026-10-08**: a node with `[revocation].enabled = true`
verifies its CA's list on load and asks it before every write.

What is still missing is the rest of the **enforcement points**: nothing in the running node
verifies a CA credential, and nothing verifies a stored occurrence's signature —
`verify_received_occurrence` exists and is tested, but there is no P2P transport that would
call it. Enrollment happens because an operator ran a CLI command, not because the node
established it.

**Implementation State:**
- ✅ **Core Cryptography (100%):** CBOR encoding, Ed25519 signing, signature verification
- ✅ **Node Identity (100%):** Key generation, persistence, `node_id` derivation
- ✅ **Signing Integration (100%):** `FullNode` signs occurrences on capture; since 2026-09-16 the v2
  payload covers every column the node authors (§5)
- ✅ **CA Credential Issuance (100% as a library, operator-driven in practice):** `ca/src/root.rs`, driven by `app ca ca-*` CLI commands
- ✅ **Revocation Checking (2026-10-08):** `RevocationWatch` loads the anchor, loads and verifies a list at startup and on a timer, and `store_occurrence` asks it about the id each row is attributed to. Enabled with no usable list is a startup error; the old flag that only logged is deleted
- ⚠️ **Verification Workflow (library yes, one runtime no):** `FullNode::verify_received_occurrence` composes key lookup, node-id derivation, signature check and recording policy; it has no caller for want of a transport
- ❌ **Peer-to-peer trust establishment (0%):** no transport, so no peer is ever verified

**The one-line version:** this system can produce proof it has not been tampered with, and since
2026-10-08 it consumes its CA's revocation statement — but not yet a peer's signature or
credential, because there is no peer link to receive them on.

---

## Component-by-Component Analysis

### 1. Canonical CBOR Encoding ✅ COMPLETE (two versions; v2 is the spec's array as of 2026-09-18)

**Status:** Fully implemented and tested. Payload **v2** added 2026-09-16 ([GAP_ANALYSIS
B7](../../GAP_ANALYSIS.md#81-blocking)) and became the specification's positional array on 2026-09-18
([B8](../../GAP_ANALYSIS.md#81-blocking)/[B9](../../GAP_ANALYSIS.md#81-blocking)); **v1** is frozen and
still accepted.

**Location:** `app/src/provenance/encode.rs`, `app/src/provenance/payload.rs`

**Implementation:**
- ✅ `CanonicalPayload` — an enum of `PayloadV1` (12 fields as a **text-keyed map**, declaration order
  preserved, unchanged on the wire) and `PayloadV2` (**an 18-element positional array**: the spec's
  elements 0–11 plus `observed_at`, `alt_m`, `accuracy_m`, `location_source`, `geo_cell_fine`,
  `geo_cell_macro`, and `signal_payload`/`advertised_name` now actually set). Neither version derives
  its serde impls: the derive would emit a map, and would spell `Vec<u8>` as an integer array
- ✅ CBOR encoding via `ciborium` crate; one encoder per version, `encode_payload` dispatching on the
  variant
- ✅ `decode_payload` reads the document's own `schema_version` (`payload_version`) and dispatches; a
  version with no known shape is `EncodeError::UnsupportedVersion`, the document's shape has to agree
  with the version it declares, and each version decodes strictly (`deny_unknown_fields` for v1's keys,
  exact arity for v2's elements), so a v1 document carrying v2 keys is refused rather than read as v1
- ✅ Determinism verified (100 iterations, all identical), and v2 now meets RFC 8949 §4.2.1 rather than
  merely claiming it: with no map in the document there is no key order to disagree about, and
  preferred serialization is checked by a head-walker written independently of the encoder, not assumed
  from the library ([B9](../../GAP_ANALYSIS.md#81-blocking), closed 2026-09-18)
- ✅ Golden bytes checked in: a v1 document captured before v2 existed (`V1_FIXTURE_HEX`, with
  `the_v1_encoder_is_untouched_by_the_new_version` asserting byte equality and
  `a_version_one_signature_still_verifies_over_its_stored_bytes` verifying a signature over it) and a
  v2 document pinned byte for byte (`version_two_bytes_are_pinned`, 245 bytes — the same document the
  spec's Complete Example prints)
- ✅ `covers_row()` states what a version attests to, so a v1 row is auditable as partial rather than
  being mistaken for a whole row
- ✅ Optional field handling (null vs value); schema version field (u16, first position)

**Alignment with Specs:**
- ✅ **Matches** [`canonical-cbor-spec.md`](../architecture/canonical-cbor-spec.md) as amended
  2026-09-18: version 2 is the spec's positional **array**, now eighteen elements, and the spec was
  brought in line with the row-coverage work rather than the reverse — elements 0–11 at the indices the
  spec has always used, the six v2 additions appended at 12–17 under its "new fields go last" rule. The
  divergence this section recorded as undecided is [B8](../../GAP_ANALYSIS.md#81-blocking), closed
  2026-09-18 in favour of the array; v1 keeps its text-keyed map and is never re-encoded, because those
  bytes are already signed
- ✅ Element order: struct declaration order is wire order, asserted rather than incidental —
  `every_element_is_the_type_the_layout_names` checks the major type at every index and
  `a_version_two_document_is_an_array_of_the_declared_number_of_elements` checks the head
- ✅ Binary fields are CBOR byte strings (major type 2). This needed writing: serde hands a bare
  `Vec<u8>` to `serialize_seq`, which spells it as an array of integers, and ciborium's reader accepts
  either, so `ByteStr`/`ByteBuf` fix the spelling on both sides and
  `a_byte_field_written_as_an_array_of_integers_is_refused` holds the line
- ✅ `adv_type` and `location_source` travel as integer **codes** whose tables live in the spec (a
  PostgreSQL enum ships no numbers); `adv_type_label`/`location_source_label` map a code back to the
  column's label, which `repo/tests/wire_types.rs` pins against `pg_enum`, and a code outside the
  tables is refused on decode

**Code Quality:**
- ✅ 36 unit tests in encode.rs, 15 in payload.rs
- ✅ Determinism verification function (`verify_determinism`), plus a head-walker (`read_head`/`heads`)
  that reads §4.2.1 conformance back off the bytes instead of taking it from the library
- ✅ Clear error types (`EncodeError`, `PayloadError`)
- ✅ Builder pattern per version; the v2 builder takes no `schema_version` — a document cannot be built
  declaring a shape it does not have

---

### 2. Ed25519 Signing ✅ COMPLETE

**Status:** Fully implemented and tested

**Location:** `app/src/provenance/sign.rs`, `app/src/node/identity.rs`

**Implementation:**
- ✅ Key generation via `ed25519-dalek`
- ✅ Node ID derivation: SHA-256(public_key) - self-certifying
- ✅ Payload signing (64-byte signatures)
- ✅ Identity persistence to JSON with 0o600 permissions
- ✅ Sign/verify integration in `NodeIdentity` struct

**Alignment with Specs:**
- ✅ `node_id = SHA-256(signing_public_key)` per provenance.md
- ✅ Separation of mTLS (transport) and signing (occurrence) keys
- ✅ Private key never leaves node (stays in memory, persisted encrypted)

**Code Quality:**
- ✅ 7 unit tests in sign.rs
- ✅ 10 unit tests in identity.rs
- ✅ File permissions set to 0o600 (owner read/write only)
- ✅ Clear separation: `sign()` and `verify()` methods on `NodeIdentity`

---

### 3. Signature Verification ⚠️ CONSUMED FOR LISTS, NOT FOR OCCURRENCES

**Status:** Core verification implemented and now run against the CA's revocation list on
load; occurrence verification exists as a method with no caller

**Location:** `app/src/provenance/verify.rs` (occurrences), `ca/src/rsl_manager.rs`
(`verified_checker`, lists), `app/src/node/revocation.rs` (who calls it)

**Implementation:**
- ✅ Ed25519 signature verification
- ✅ Tamper detection (payload modification)
- ✅ Wrong key detection
- ✅ Error handling with `VerifyError`
- ✅ List verification, run in production since 2026-10-08: `ca::verified_checker` refuses an
  unsigned, mis-issued, tampered or expired list rather than building a checker from it
- ❌ Composed workflow `verify_occurrence_with_revocation` and the `should_record_data` /
  `should_allow_connection` helpers — deleted 2026-10-08. They duplicated the policy decision
  with hardcoded staleness and treated `AcceptWithWarning` as a rejection; `RevocationWatch`
  is now the single place the node asks

**Missing at runtime:**
- ❌ No caller verifies a stored occurrence's signature — no `verify` subcommand, no
  query-time flag
- ❌ The node never verifies a CA credential against a CA root key. `CaRoot::verify_credential`
  is reachable only from the `ca-verify` CLI
- ❌ No CA root key or anchor is loaded for credential checking. The *revocation* anchor is
  configured now (`[revocation].anchor_path`), and nothing else reads it — which is why
  credential checking is not simply "reuse the anchor": authentication must not depend on a
  revocation switch being on
- ❌ `VerifyError::InvalidSignature` and `VerifyError::InvalidPublicKey` are never constructed
  anywhere — the compiler is reporting that the failure arms of this workflow are unreachable

**Note on `FullNode::verify_received_occurrence`:** until 2026-10-08 this verified the
signature with `self.identity` — *this* node's own public key — with the caller's
`_signing_public_key` explicitly unused. Correct for a self-check, wrong for a peer's
occurrence, and latent only because there was no caller. It now reads the peer's key from
`nodes.signing_public_key`, refuses a registry row whose key does not hash to the node id it
is keyed by, and verifies with `verify_strict`. Still no caller: no P2P transport.

**Code Quality:**
- ✅ 9 unit tests in verify.rs
- ✅ Clear error types (`VerifyError`)
- ✅ Proper Ed25519 verification via `ed25519-dalek`

---

### 4. Node Identity Management ✅ COMPLETE

**Status:** Fully implemented

**Location:** `app/src/node/identity.rs`

**Implementation:**
- ✅ Key generation (random Ed25519 keypairs)
- ✅ Identity persistence (JSON file, 0o600 permissions)
- ✅ Load-or-create pattern for first-run
- ✅ Sign/verify methods
- ✅ Node ID derivation

**Alignment with Specs:**
- ✅ Self-certifying node IDs (SHA-256 of public key)
- ✅ No separate registry lookup needed
- ✅ Persistent across node restarts

**Code Quality:**
- ✅ 10 comprehensive unit tests
- ✅ Proper file permission handling (Unix-specific)
- ✅ Clear error handling

---

### 5. FullNode Integration ✅ COMPLETE (row-first, v2, as of 2026-09-16)

**Status:** Fully implemented

**Location:** `app/src/node/full.rs`

**Implementation:**
- ✅ Builds the **row** first, then derives the attestation from it
  (`PayloadV2::from_occurrence(&occurrence)`), so the signature is a function of the stored columns
  rather than a parallel field list a future edit could pull apart
- ✅ Truncates both timestamps to microseconds **once**, before either is built — `TIMESTAMPTZ` keeps
  microseconds, and signing a finer value would sign something the row cannot show
- ✅ CBOR encodes payload, signs the encoded bytes with the node identity, and stores
  `signed_payload`, `signature` and `schema_version = 2`
- ✅ Carries `advertised_name` into the row (it was dropped before v2, and it is the column most likely
  to carry personal data), and states the no-fix `location_source` label rather than inheriting a default
- ✅ Rate limiting before signing (efficiency)

**What the signature does not cover, and why it is written down rather than discovered:**
`occurrence_id` (node does not generate it — [M21](../../GAP_ANALYSIS.md#82-major)), `ingested_at`
(written by whoever stores the row), `adv_type` and `tx_power` (signed as absent; `bt_mon` reports
neither — [M22](../../GAP_ANALYSIS.md#82-major)), and any location an aggregator adds afterwards.

**Alignment with Specs:**
- ✅ Signing happens at capture time (not reconstruction)
- ✅ Both `signed_payload` and `signature` stored verbatim
- ✅ `origin_node_id` derived from signing key

**Code Quality:**
- ✅ Architecture diagram in module docs
- ✅ Comprehensive error handling
- ✅ Statistics tracking
- ✅ 12 tests, incl. `the_signature_names_every_column_the_node_authors` (payload field ↔ row column,
  including the H3 cells re-derived through `repo::geo`) and
  `every_column_the_node_authors_changes_the_signature` (ten columns altered one at a time; under v1
  six of them left the bytes untouched)

---

### 6. Database Schema ⚠️ PARTIAL

**Status:** Schema supports provenance; the invariants that make it trustworthy are unenforced

**Location:** `db/src/migrations/202607312146_create_nodes.sql`,
`db/src/migrations/202607312137_create_bluetooth_occurrence_types.sql`,
`db/src/migrations/202608260000_create_node_revocations.sql`

**Implemented:**
- ✅ `nodes` table with `signing_public_key` (BYTEA, NOT NULL)
- ✅ `nodes` table with `ca_credential` (BYTEA, NOT NULL)
- ✅ `nodes` table with `status` (`node_status` enum)
- ✅ `occurrences` table with `signed_payload` (BYTEA, NOT NULL)
- ✅ `occurrences` table with `signature` (BYTEA, NOT NULL)
- ✅ `nodes` table with `node_id` as PRIMARY KEY (BYTEA)
- ✅ `node_revocations` table with reason codes, `rsl_sequence_number`, and audit columns

**Resolved since 2026-08-26:** the `node_type`, `node_status`, `signal_type`,
`location_source`, `adv_type`, `ble_address_type` and `sync_direction` enums referenced by the
tables **are** defined — in `202607312137_create_bluetooth_occurrence_types.sql`, which
migrates before the tables that use them. The August note flagging them as "referenced but not
defined" was wrong.

**Still missing:**
- ❌ No constraint or trigger ensuring `node_id = SHA-256(signing_public_key)`. The
  self-certifying identity model is enforced only by `app/src/node/identity.rs`; a row written
  by any other path can break it
- ❌ No `node_id` length CHECK (32 bytes) and no `device_hash` length CHECK
- ❌ Nothing keeps `nodes.status = 'revoked'` in step with a `node_revocations` row; the
  migration comment says it "should" be done, no code does it
- ❌ No migration backfilling `ca_credential` for rows written before enrollment existed
- ⚠️ Unexercised: no database has ever run these migrations, so not one of these guarantees has
  been observed rather than inferred

**Schema Comments:**
- ✅ Good documentation on CA credential purpose
- ✅ Clear explanation of offline verification

---

### 7. CA Enrollment Workflow ⚠️ OPERATOR-DRIVEN, NOT NODE-DRIVEN

**Status:** Implemented as a library plus CLI; the node itself never participates

**Per provenance.md requirements:**
1. Node generates Ed25519 keypair locally — ✅ `app/src/node/identity.rs`
2. Public key is submitted for credentialing — ✅ manually: `app ca ca-enroll` reads the node's key
3. CA issues a credential over the key — ✅ `CaRoot::issue_credential` (`ca/src/root.rs`)
4. Credential and public key are stored in `nodes` — ✅ `NodeRepository::register` writes both, from the CLI path (`app/src/cli.rs`)
5. Credential is verified — ✅ but only when an operator runs `app ca ca-verify`
6. Credential propagates to the rest of the network via sync — ❌ no sync exists (Phase 1)

**What exists:**
- ✅ `ca/src/root.rs` — `CaRoot::generate`, `load_from_file`, `issue_credential`, `verify_credential`, `revoke`, `generate_rsl`, `verify_rsl`
- ✅ `ca/src/credential.rs` — credential structure and its signing/verification
- ✅ CLI: `ca-init`, `ca-enroll`, `ca-verify`, `ca-revoke`, `ca-info`, `ca-generate-rsl`
- ✅ `[ca]` configuration section in `config.example.toml`

**What does not exist:**
- ❌ Enrollment at node startup. A node with no `nodes` row refuses to scan (`ensure_node_registered`)
  rather than enrolling itself, which is the right failure mode but leaves enrollment manual
- ❌ An enrollment protocol between nodes — no wire format, no request/response, no trust
  handshake. The "CA" today is a root key file on one operator's disk plus a CLI
- ❌ Any external CA integration (step-ca, Vault PKI)
- ❌ Node-side credential verification, as covered in section 3

**Security impact: still HIGH, for a different reason than the August analysis gave.**
Credentials are genuinely issued and genuinely verifiable, so the trust anchor exists. The
problem is that verifying it is optional: nothing forces a receiving node to check a
presented credential, and there is no peer channel on which one would be presented.

---

### 8. Offline Verification Workflow ⚠️ COMPONENTS EXIST, NEVER COMPOSED AT RUNTIME

**Status:** Every step has an implementation; no code path runs them in sequence

**Per provenance.md requirements:**
```
Given an occurrences row:
1. Look up node: nodes WHERE node_id = occurrence.origin_node_id
2. Verify CA credential (once per node, cacheable)
3. Verify signature (per-occurrence)
4. Check revocation status: nodes.status != 'revoked'
5. If all checks pass: Row confirmed from origin_node_id
```

**Current state, step by step:**
- ✅ Step 1 — `NodeRepository` lookups exist
- ⚠️ Step 2 — `CaRoot::verify_credential` exists and is tested, but is invoked only from the
  `ca-verify` CLI. No node loads a CA root, and no credential verification is cached
- ✅ Step 3 — `verify.rs` and `verify_signature_only` exist
- ⚠️ Step 4 — `InMemoryRslChecker::is_revoked` exists. `FullNode` holds one, but `FullNode::new`
  constructs it **empty** and `enable_revocation_checking` only logs; nothing loads an RSL from
  `node_revocations` or from `DatabaseRslManager`. Checking an empty list returns `Unknown` for
  every node, which the lenient recording policy accepts — `Unknown` being the behaviour as of
  2026-09-14 ([B6](../../GAP_ANALYSIS.md#81-blocking)); before that an empty checker answered `Valid`
  for every node on earth. `FullNode` does not consult a `RevocationPolicy`, it matches the status
  itself, and its `Unknown` arm stores the occurrence with a warning
- ⚠️ Step 5 — `verify_occurrence_with_revocation` composes exactly these steps and is
  unit-tested doing so. Nothing in `app/src` outside that test module calls it

**Missing to close it:**
- ❌ CA root key in node configuration, loaded at startup
- ❌ Per-peer credential verification with a cache
- ❌ An RSL loader: `DatabaseRslManager` (already written) wired to a checker and refreshed
- ❌ A read-side entry point — a `verify` CLI subcommand or query-time verification flag
- ❌ Keeping `nodes.status` consistent with `node_revocations`

**Practical consequence:** asking "is this row authentic?" today returns no answer, even though
every primitive needed to answer it exists and passes its own tests.

---

### 9. Signing Key Rotation ❌ NOT IMPLEMENTED

**Status:** No implementation or policy

**Per provenance.md Open Questions:**
> "Should signing keys rotate on similar cadence as mTLS certs?"

**Current State:**
- ❌ No key rotation mechanism
- ❌ No rotation policy defined
- ❌ No key history tracking in schema
- ❌ No dual-key transition period support

**Security Impact:** MEDIUM
- Long-lived keys increase compromise risk
- No forward secrecy for historical occurrences

---

### 10. Aggregator Enrichment Signature Layer ❌ NOT IMPLEMENTED

**Status:** Not implemented (marked as open question in spec)

**Per provenance.md Limitations:**
> "Should aggregator enrichment get its own signature layer (second signature over enriched fields, by reporting_node_id) to close this gap?"

**Current State:**
- ❌ No second signature for aggregator-added fields
- ❌ Aggregator location/timestamp trusted only via `reporting_node_id` identity

**Design Decision Needed:**
- This is an architectural choice, not a bug
- Current implementation trusts aggregators via their verifiable node identity
- Closing the gap would require double-signing architecture

---

## Security Analysis

### Critical Gaps

1. **Trust anchor exists but is never consulted at runtime** ⚠️ HIGH
   - A CA root key file plus the CLI issues and verifies real credentials, so "any node can
     claim any identity" is no longer strictly true of the *issuance* path
   - It remains true of the *consumption* path: no node verifies a presented credential, so
     nothing is actually gained until verification is wired into a receive path
   - **Impact:** provenance is currently write-only

2. **Revocation cannot reject anything** ⚠️ HIGH
   - `node_revocations` is written by `ca-revoke`, RSLs can be generated and signed, and the
     checker enforces staleness correctly — in tests
   - At runtime the checker is empty, so every node resolves to `Unknown`, which the lenient
     recording policy accepts and the strict connection policy would reject. Since no peer
     connections exist either, the strict policy never runs
   - **Impact:** a compromised node's key is as valid as any other's until an operator both
     revokes it and manually loads a list nothing loads by itself

3. **No Key Rotation** ⚠️ MEDIUM
   - Long-lived keys increase attack surface
   - No forward secrecy
   - **Impact:** Long-term compromise affects all historical data

### Medium Concerns

4. **Private Key Storage** ⚠️ MEDIUM
   - JSON file with 0o600 permissions
   - No encryption at rest
   - No HSM integration path
   - **Impact:** Disk compromise = key compromise

5. **No Public Key Pinning** ⚠️ MEDIUM
   - node_id derivation is correct
   - But no pinning mechanism for initial trust
   - **Impact:** First-use trust, susceptible to MITM on initial enrollment

### Good Security Practices Implemented

✅ **Separation of mTLS and signing keys** - Limits blast radius
✅ **Self-certifying node IDs** - No central registry needed
✅ **Deterministic CBOR encoding** - Prevents canonicalization attacks
✅ **Signature stored verbatim** - No reconstruction ambiguity
✅ **File permissions 0o600** - Restrictive access
✅ **Private key never logged** - Proper secret handling

---

## Inconsistencies and Issues

### 1. Spec Reference Inconsistency

**Location:** `app/src/provenance/mod.rs`

```rust
// Says: roadmap/phase_0/canonical-payload-spec.md
// But field order comments say: 0-11 (12 fields)
// Actual struct has 12 fields ✅ CORRECT
```

**Status:** ✅ Resolved 2026-09-18. There is no longer one 12-field struct: `PayloadV1` keeps those
12 fields as a text-keyed map and `PayloadV2` is the specification's **positional array of eighteen
elements** (`observed_at`, `alt_m`/`accuracy_m`/`location_source`, the two `geo_cell_*` appended at
12–17), each version documenting its own layout in `payload.rs` and the spec documenting both. The
specification and the implementation now describe one format, which is what
[B8](../../GAP_ANALYSIS.md#81-blocking) existed to force a decision about.

---

### 2. Signal Type Field Documentation

**Location:** `app/src/provenance/payload.rs`

```rust
/// Encoding: u8 with the following values:
/// - 0 = Bluetooth
/// - 1 = WiFi
/// - 2 = NFC
/// - 3 = Zigbee
/// - 4 = LoRaWAN
```

**Issue:** Spec says u8 but `CanonicalPayload` struct uses `u8` directly. The spec in `canonical-payload-spec.md` also says `u8`. **This is consistent.** Both `PayloadV1` and `PayloadV2` keep `signal_type: u8`, and v2 encodes it with the same `signal_type_code`, so a v1 and v2 row describe the same signal the same way.

---

### 3. Location Field Semantics

**Location:** Multiple

**Architecture Spec:** Location only included if from origin node GPS/fixed
**Implementation:** ✅ Correct - `location` field is optional, aggregator-added location not covered by signature. Since 2026-09-16 v2 also signs `location_source`, so a reader can tell `node_gps` from `aggregator_fixed` instead of inferring it from the presence of a coordinate, and signs the `geo_cell_fine`/`geo_cell_macro` derived from that same point (`the_signature_covers_the_location`).

---

### 4. device_hash Format

**Location:** `canonical-cbor-spec.md` vs implementation

**Spec Says:** "Raw bytes (32 bytes) for device_hash and origin_node_id"
**Implementation:** ✅ `Vec<u8>` in both `PayloadV1` and `PayloadV2`

**Status:** ✅ Consistent, and now consistent on the wire too. v2 sends every `Vec<u8>` field
(`origin_node_id`, `device_hash`, `device_address`, `signal_payload`) as a CBOR **byte string** — major
type 2, what the spec's "raw bytes" means — through the `ByteStr`/`ByteBuf` wrappers, because serde's
default for a `Vec<u8>` is an *array of integers* (major type 4) and ciborium's reader will happily read
either spelling back. That the array form is refused rather than tolerated is
`a_byte_field_written_as_an_array_of_integers_is_refused`. v1 still emits integer arrays and still
decodes them: its bytes are signed and frozen. Closed with
[B8](../../GAP_ANALYSIS.md#81-blocking) 2026-09-18.

---

### 5. Enum Definitions — RESOLVED

**Location:** `db/src/migrations/202607312137_create_bluetooth_occurrence_types.sql`

The `node_type` and `node_status` types referenced by `create_nodes.sql` are created in that
earlier migration, together with `signal_type`, `ble_address_type`, `location_source`,
`adv_type` and `sync_direction`. Migration ordering (2137 before 2146) is correct, so the
tables resolve their types.

The open item in this area is no longer missing types but missing *values*: `location_source`
has no `unknown` variant, which is why a row with no fix must still assert one. See
`geo-h3-alignment.md` D4.

---

### 6. CA Credential Distribution

**Architecture Spec Says:**
> "Propagate to rest of network through normal node-registry sync (NOT re-fetched from CA at verify time)"

**Implementation:** ❌ No sync mechanism implemented yet

**Status:** Blocked on Phase 1 (Two-Node Sync)

---

## Roadmap Alignment

### Current Roadmap (from `roadmap.md`)

**Cross-Cutting Research Items - Provenance / Signature Verification:**
```
- [ ] Finalize exact canonical_encode byte layout/field order (BLOCKING for Phase 0)
- [ ] Signing-key rotation cadence
- [ ] Aggregator enrichment signature layer (optional)
```

**Assessment:**
- ✅ "Finalize canonical_encode" - **DONE** (specs approved, implemented)
- ❌ "Signing-key rotation cadence" - **NOT ADDRESSED**
- ❌ "Aggregator enrichment signature" - **NOT DECIDED** (open question)

**Now addressed since 2026-08-26:**
- ✅ CA credential issuance — `ca/src/root.rs`, driven by `app ca ca-enroll`
- ✅ Revocation data structures and RSL generation/signing
- ✅ Policy framework for recording vs. connection decisions

**Still missing, and this is the honest list:**
- ❌ Node-side credential verification (the code exists; the call site does not)
- ❌ CA root key in node configuration, loaded at startup
- ❌ An RSL loader that populates a checker from `node_revocations` / `DatabaseRslManager`
- ❌ A composed verification entry point reachable by an operator or a peer
- ❌ Enrollment as a protocol rather than a CLI invocation

---

## Recommendations

### Immediate (Phase 0 Blocking)

Ordered so each step produces an observable result rather than another untested library.

1. **Load the trust anchor**
   - Add the CA root key path to configuration (the `[ca]` section already exists)
   - Load it at node startup and fail loudly when revocation checking is enabled but no root is configured

2. **Verify one credential, then cache it**
   - On first contact with a peer's `node_id`, verify `ca_credential` with `CaRoot::verify_credential`
   - Cache the result per node, as the architecture already prescribes
   - Surface failures in stats, not only logs

3. **Populate the revocation checker**
   - Wire `DatabaseRslManager` (already implemented) to `InMemoryRslChecker`
   - Refresh on a schedule and expose cache age — the staleness logic is already written for it

4. **Add one read-side verification command**
   - There is no HTTP server to add an endpoint to, so start with `app verify <occurrence-id>`
     running the full five-step workflow and printing the verdict
   - That single command converts "primitives that pass unit tests" into a demonstrable property

### Short-Term (Before Production)

5. **Define Key Rotation Policy**
   - Frequency (e.g., 90 days like TLS certs?)
   - Rotation mechanism (dual-key transition?)
   - Historical occurrence verification during transition

6. **Expose Verification as a Service**
   - Once an HTTP layer exists, add `GET /occurrences/{id}/verify` returning
     `{ valid, origin_node_id, verified_at }` over the full workflow
   - Until then, the `verify` CLI subcommand from step 4 is the interface — do not document an
     endpoint the binary does not serve

7. **Decide on Aggregator Enrichment**
   - Keep current: trust via `reporting_node_id`
   - Or add double-signing: a second signature over enrichment fields by the aggregator
   - Document the decision in the architecture either way

### Medium-Term (Phase 5-6)

8. **CA Automation**
   - Integrate with step-ca or Vault PKI
   - Automated certificate renewal
   - Bulk enrollment for fleet deployment

9. **Key Storage Hardening**
   - Consider encrypted file storage
   - HSM integration path for high-security deployments
   - Key backup/recovery procedure

---

## Testing Gaps

### Missing Tests

1. **Cross-Platform Determinism**
   ```rust
   // Encode on Linux, decode on macOS, verify identical bytes
   // Requires reference CBOR bytes stored in repo
   ```
   🔶 Reference bytes are checked in for both versions (`V1_FIXTURE_HEX`, and `V2_FIXTURE_HEX` asserted
   by `version_two_bytes_are_pinned`), and since 2026-09-18 they are reference bytes for a *specified*
   format rather than for one Rust encoder: v2 is the spec's 18-element array, byte strings are byte
   strings, and §4.2.1 preferred serialization is asserted by a reader written independently of the
   writer. A second implementation can now be checked against the document and the fixture instead of
   against observed output — [B8](../../GAP_ANALYSIS.md#81-blocking)/
   [B9](../../GAP_ANALYSIS.md#81-blocking), both closed 2026-09-18. What remains untested is the
   tautological part: no second implementation exists yet, so "two implementations agree" is still an
   argument, not a result.

2. **CA Credential Verification**
   ```rust
   // Test valid credential
   // Test invalid credential (tampered)
   // Test expired credential (if time-based)
   ```

3. **Revocation Checking**
   ```rust
   // Test occurrence from revoked node
   // Test occurrence from active node
   // Test offline verification (no fresh revocation check)
   ```

4. **Integration Test**
   ```rust
   // Full cycle: Generate identity → Enroll with CA → Sign occurrence
   // → Store in DB → Verify occurrence (all steps)
   ```

---

## Conclusion

**Implementation Status: PARTIALLY DONE (~60%)**

The cryptographic foundation is solid and well-tested. However, the **trust infrastructure (CA enrollment, verification, revocation)** is missing, which is essential for the security model described in `provenance.md`.

**Blocking Issues:**
1. No CA enrollment mechanism
2. No credential verification
3. No revocation checking

**Recommendation:** Complete CA infrastructure before Phase 0 exit. The core crypto is ready, but without CA trust anchors, the system has no security guarantees against identity spoofing.

---

## References

- **Architecture:** [`provenance.md`](../architecture/provenance.md)
- **CBOR Spec:** [`canonical-cbor-spec.md`](../architecture/canonical-cbor-spec.md)
- **Phase 0 Spec:** [`canonical-payload-spec.md`](./roadmap/phase_0/canonical-payload-spec.md)
- **Roadmap:** [`roadmap.md`](./roadmap.md)
