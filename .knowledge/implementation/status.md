# Implementation Status

Component-by-component status of the workspace against the architectural plan.

**Last updated:** September 2, 2026
**Verification basis:** every claim below was read from the working tree or produced by a
command run in the dev container on 2026-09-02. Anything that could not be executed here
(needing Postgres, PostGIS, h3-pg, or a Bluetooth adapter) is marked **unexercised**.

> Deep, per-component validation against the *fully implemented* end state lives in
> [`GAP_ANALYSIS.md`](../../GAP_ANALYSIS.md). This document is the summary; that one is the
> evidence. Where the two disagree, `GAP_ANALYSIS.md` is newer.

---

## Workspace

Six crates: `app` (node binary), `bt_mon` (BLE scanning library), `bt_iden` (probabilistic
identity resolution), `repo` (data layer), `db` (migration tool), `ca` (CA library).

There is **no HTTP API and no web framework in the dependency tree** — no axum, hyper, or
actix in any `Cargo.toml`. The root `README.md` claim "Web Framework: Axum" is aspirational,
not current. The only operator interface is the `app` CLI.

---

## Phase Roadmap

| Phase | Description | Status | Notes |
|-------|-------------|--------|-------|
| **Phase 0** | Single-node prototype | **In progress — code-complete, never validated live** | Scan → sign → store works in code; no live capture has ever run |
| **Phase 0.5** | CA infrastructure | **Partially done** | CA library + CLI enrollment exist; the node runtime never uses them |
| **Phase 1** | Two-node sync | **Not started** | `rust-mqtt` is a declared `app` dependency with zero references in `app/src` |
| **Phase 2** | Federated query layer | **Not started** | Depends on Phase 1 |
| **Phase 3** | Association detection | **Not started** | `bt_iden` exists as an isolated library; nothing consumes it |
| **Phase 4** | Signal/aggregator/LoRa tier | **Not started** | `SignalPing` wire format not designed |
| **Phase 5** | Scale to N full nodes | **Not started** | No gossip implementation, design doc only |
| **Phase 6** | Hardening | **Not started** | No mTLS, no access control, no metrics export |

---

## Component Status

### 1. Bluetooth Scanning Layer (`bt_mon`)

Feature gates: `default = ["btleplug"]`, plus `bluer`, `mock`, `full`.

| Feature | Planned | Implemented | Notes |
|---------|---------|-------------|-------|
| Cross-platform BLE scanning | Yes | Yes | `backends/{btleplug,bluer,mock}.rs` |
| Device discovery | Yes | Yes | Implemented |
| GATT client operations | Yes | Yes | Read/write/subscribe |
| Raw advertisement capture | Partial | Partial | Device info exposed; raw payload fields not surfaced in the API |
| Advertisement type detection | Partial | Partial | `UpdateField` enum exists; no full BLE AD-structure parser |
| Mock backend | — | Yes | Behind the `mock` feature |

**Status:** ✅ **Functional for Phase 0** — 26 lib tests pass.

**Defect:** `bt_mon/examples/mock_backend_demo.rs` imports `bt_mon::backends::mock` but the
example declares no `required-features = ["mock"]`, so `cargo check --workspace
--all-targets` fails with E0432 unless `--features mock` is passed. Any CI running
`--all-targets` is red.

**Gaps:**
- Raw advertisement payload still not exposed for storage in `occurrences.signal_payload`.
- No AD-structure parsing to populate `ble_address_type` / `adv_type` faithfully.
- RSSI sampling rate is not configurable, which rate-limit validation needs.

---

### 2. Identity Resolution (`bt_iden`)

| Feature | Status |
|---------|--------|
| Fingerprint matching, temporal adjacency, resolver | Implemented as a standalone library |
| Config + models | Implemented |
| Tests | 41 passing (10 unit + 19 integration + 12 property) |
| **Consumers** | **None** — no other crate depends on `bt_iden` |

**Status:** ✅ **Internally complete, externally unwired.** This crate appeared in no status
document before 2026-09-02. It is Phase 3 groundwork that nothing calls yet.

---

### 3. Database Schema (`db` crate)

Seven migrations, all authored, **none ever executed** (see Environment limits).

| Migration | Creates |
|-----------|---------|
| `202607312127_add_extensions` | pgcrypto, postgis, postgis_raster, h3 |
| `202607312137_create_bluetooth_occurrence_types` | `signal_type`, `node_type`, `node_status`, `ble_address_type`, `location_source`, `adv_type`, `sync_direction` |
| `202607312146_create_nodes` | `nodes` + 3 indexes |
| `202607312147_create_bluetooth_occurrences` | `occurrences` (partitioned), `occurrence_relays`, 2 partitions |
| `202608020206_create_occurrence_indexes` | 5 occurrence indexes + 3 relay indexes |
| `202608020247_create_sync_cursors` | `sync_cursors` |
| `202608260000_create_node_revocations` | `node_revocations` + 3 indexes |

| Table | Planned | Implemented | Notes |
|-------|---------|-------------|-------|
| `nodes` | Yes | Yes | `node_id BYTEA PK`, `signing_public_key`, `ca_credential NOT NULL`, `owns_geo_cells H3INDEX[]` |
| `occurrences` | Yes | Yes | Unified signal-type table; `signed_payload`/`signature NOT NULL`; generated H3 columns |
| `occurrence_relays` | Yes | Yes | Modelled in `repo`, **unused at runtime** |
| `sync_cursors` | Yes | Yes | Table only; no application logic reads or writes it |
| `node_revocations` | Yes | Yes | Written only by the `ca-revoke` CLI path |
| `occurrence_rollups` | Yes | **No** | Never created |
| `device_identities` | Yes | **No** | Never created |
| `device_address_links` | Yes | **No** | Never created |
| `co_occurrence_events` | Yes | **No** | Never created |
| `association_edges` | Yes | **No** | Never created |

**Status:** ⚠️ **Authored, unvalidated, and currently unable to accept a row dated today.**

**Blockers found 2026-09-02:**
1. **No partition covers the current month.** Only `occurrences_2026_07` and
   `occurrences_2026_08` exist. Any `INSERT` with `observed_at` in September 2026 or later
   fails with `no partition of relation "occurrences" found for row`. The migration comment
   says "automate creation via cron/pg_partman in practice" — nothing does.
2. `location_source` has no `unknown` variant (`node_fixed`, `node_gps`, `interpolated`,
   `aggregator_fixed`). A row with no fix is forced to assert a location source it does not
   have. See `geo-h3-alignment.md` D4.
3. Nothing enforces `node_id = SHA-256(signing_public_key)` in SQL. The invariant the
   self-certifying identity model rests on is enforced only in Rust.
4. `nodes` has no `display_name` or `short_id` column, both described as part of node
   identity in `architecture/overview.md`.
5. Partitioning is time-only; geo-sharding is delegated to the application layer, which does
   not implement it.

---

### 4. Repository Layer (`repo` crate)

| Feature | Planned | Implemented | Notes |
|---------|---------|-------------|-------|
| Connection pool | Yes | Yes | `pool.rs`, `RepoError` |
| Type-safe models | Yes | Yes | `Occurrence` (unified), `OccurrenceRelay`, `Node`, `RevokedNode`, `SignalType`, typed enums |
| Generic repository pattern | Yes | Yes | `Executor` trait; works inside transactions |
| Provenance columns | Yes | Yes | `signed_payload`, `signature`, `schema_version`, `origin_node_id` all modelled |
| PostGIS location | Yes | Yes | `PostgisPoint`, `geo-types` with serde |
| H3 client-side derivation | Yes | **Yes (new)** | `repo/src/geo.rs`, 534 lines; `h3o` is now genuinely used |
| Partition-aware queries | n/a | n/a | Postgres routes partitions |

**Status:** ✅ **Aligned with the schema.** The model/schema misalignment that every document
dated August 2026 called the most significant divergence in the codebase has been fixed. See
[`repo-divergence.md`](repo-divergence.md).

**Tests:** 37 passing, 2 failing — both `revocation_repo` tests requiring `DATABASE_URL`.

---

### 5. Application Layer (`app` crate)

| Feature | Planned | Implemented | Notes |
|---------|---------|-------------|-------|
| Node identity (Ed25519, `node_id` derivation) | Yes | Yes | `node/identity.rs`; persisted under `data_dir` with restrictive permissions |
| Sign occurrences on capture | Yes | Yes | `node/full.rs` builds the canonical payload, CBOR-encodes, signs, stores |
| Rate limiting | Yes | Yes | `node/rate_limiter.rs`; DashMap, 15 s default threshold, configurable cache cap |
| Clock discipline (Phase 0 minimal) | Yes | Partial | `SystemClock`; `now()` and `now_local()` are the same reading; no drift measurement |
| Position acquisition | Yes | Yes | `position/`: fixed, GPS, mock, none — wrapped in `BestEffortPositionSource` |
| Layered configuration | Yes | Yes | `config/{flags,file,env_file,layered}.rs`; precedence flag > file > env > default |
| CLI | Yes | Yes | `monitor`, `query`, `stats`, and CA commands nested under `ca`: `ca-init`, `ca-enroll`, `ca-verify`, `ca-revoke`, `ca-info`, `ca-generate-rsl` |
| Enrolled-node precondition | Yes | Yes | `ensure_node_registered()` fails fast with the fix command instead of emitting per-row FK errors |
| Device identity hashing | Yes | Yes | `node/device_id.rs` |
| Self-enrollment at startup | Yes | **No** | Enrollment is a manual CLI step; the node refuses to scan rather than enrolling itself |
| HTTP API | Yes (per README) | **No** | No web framework in the tree |
| Metrics / observability | Yes | Partial | Atomic counters, `FullNodeStats`, `stats` CLI; no exporter |

**Status:** ⚠️ **Core Phase 0 path implemented; the trust path is not wired into it.**

**Defect:** `app/src/node/mod.rs:141` `test_now_equals_now_local_phase0` asserts
`now() == now_local()` byte-exactly and fails intermittently by a few hundred nanoseconds.
The Phase 0 invariant is "same clock", which needs a tolerance, not equality.

---

### 6. Provenance, CA, and Revocation

Split across `app/src/provenance/`, the `ca` crate, and `node/full.rs`.

| Capability | Status | Evidence |
|------------|--------|----------|
| Canonical CBOR payload (v1, 12 fields, fixed order) | ✅ Done, tested | `provenance/{payload,encode}.rs`; determinism verified over repeated iterations |
| Ed25519 sign / verify | ✅ Done, tested | `provenance/{sign,verify}.rs` |
| Sign on capture, store verbatim | ✅ Done | `node/full.rs` |
| Self-certifying node identity | ✅ Done in Rust | `node/identity.rs`; **not** enforced in SQL |
| CA root generation / persistence | ✅ Done | `ca/src/root.rs` (`generate`, `load_from_file`) |
| Credential issuance + verification | ✅ Done in crate and CLI | `issue_credential` / `verify_credential`, driven from `cli.rs` |
| Revocation data structures (RSL, builder, reasons) | ✅ Done | `ca/src/revocation.rs` |
| Revocation policies (recording lenient / connection strict) | ✅ Done, and applied by the node since 2026-10-08 | `ca/src/revocation.rs` defines them; `app/src/node/revocation.rs` is what applies them. The parallel implementation in `provenance/verify_with_revocation.rs` (hardcoded staleness, `AcceptWithWarning` treated as a rejection, no caller) was deleted rather than kept as a second answer |
| RSL storage managers | ✅ Done, used by `ca-generate-rsl` since 2026-09-14 | `ca/src/rsl_manager.rs` (`DatabaseRslManager`, `InMemoryRslManager`) publishing into `revocation_status_lists`, with a durable per-CA sequence number (`ca/tests/rsl_persistence.rs`). Both sides now: `ca::verified_checker` is the read path, and a node installs what it returns — see the next two rows |
| **Node verifies a CA credential** | ❌ **Not wired** | `verify_credential` is called only from the `ca-verify` CLI, never by `FullNode`. Closed for **lists**: `ca::verified_checker` verifies every list against the configured `TrustAnchor` on the way in, so a stored list is proven to be the CA's (M11, M12) |
| **Node checks revocation at runtime** | ✅ **Done 2026-10-08** | `FullNode` holds `Option<Arc<RevocationWatch>>` (`app/src/node/revocation.rs`): loads the anchor in `[revocation].anchor_path`, loads and verifies a list at startup and on `[revocation].refresh_secs`, and `store_occurrence` asks it about the id the row will be attributed to. Enabled with no usable list is a startup error, not a warning. Off is still the default, and the watch is `None` then — read as *no gate*, not as a refusal |
| **Combined verification workflow** | ⚠️ **Partly** | `FullNode::verify_received_occurrence` verifies a peer's signature under the key registered for that node id and then applies the recording policy, and `should_allow_peer_connection` applies the connection policy. Neither has a caller: there is no P2P transport to receive an occurrence or a handshake. The old free functions (`verify_occurrence_with_revocation`, `should_record_data`, `should_allow_connection`) are deleted |
| Payload v2 with signed geo fields | ✅ Done | `PayloadV2` is what the node writes (`CURRENT_VERSION`); v1 is decoded and accepted, never produced. `adv_type`/`tx_power` stay absent from the signature because nothing populates them — see GAP_ANALYSIS M22 |
| Signing-key rotation | ❌ Not started | No mechanism, no policy |
| mTLS | ❌ Not started | Phase 6 |

**Status:** ⚠️ **~75% — cryptography complete and enforced against revocation; a node still
cannot prove its CA issued the key it signs with.**

The nuance that matters for Phase 0: a node writes cryptographically valid signatures, and
since 2026-10-08 it consults a verified CA revocation list before writing them. What the read
path still does not do is verify a stored occurrence's signature — no query command checks one,
and `verify_received_occurrence`, which would be the call site, has no caller because there is
no P2P transport. No node validates a peer's CA credential either: `ca ca-verify` is an operator
command, not part of a handshake. The primitives are ready; two of the enforcement points exist,
two do not.

---

### 7. MQTT / Networking

| Feature | Status |
|---------|--------|
| MQTT client | ❌ Not started — `rust-mqtt = "0.5.1"` declared in `app/Cargo.toml`, referenced nowhere in `app/src` |
| Topic design | ❌ Not started |
| Peer-to-peer sync | ❌ Not started |
| `sync_cursors` logic | ❌ Not started — table exists, no reader or writer |
| Credential distribution via sync | ❌ Not started |

**Status:** ❌ **Not started.** Phase 1. Reference material in `architecture/mqtt-v5.md`.

---

### 8. Gossip Protocol

| Feature | Status |
|---------|--------|
| SWIM implementation | ❌ Not started |
| Node membership | ❌ Not started — `nodes.last_seen_at` / `nodes.status` are never written by any code path |
| Failure detection | ❌ Not started |

**Status:** ❌ **Not started.** Phase 5. Design notes in `architecture/tarantool-swim.md`.

---

### 9. H3 Geo-indexing

| Feature | Planned | Implemented | Notes |
|---------|---------|-------------|-------|
| `h3-pg` extension | Yes | ⚠️ Unexercised | Signatures checked against PGXN docs, never run |
| `h3o` Rust crate | Yes | **Yes** | `repo/src/geo.rs`; res 9 fine, res 6 as parent-of-fine |
| Generated columns | Yes | Yes (schema) | Derived by Postgres at INSERT |
| Node geo-ownership | Yes | Partial | `node_repo::register` accepts `owns_geo_cells`; derivation at enrollment (`geo-h3-alignment.md` D2) only partly landed |
| Row ↔ payload geo consistency | Yes | **No** | Payload carries no cell, so the DB derivation cannot be cross-checked |
| H3 partitioning / re-sharding | Yes | No | Not implemented |

**Status:** ⚠️ **Client-side derivation now exists; signing it does not.**
Workstream status in [`geo-h3-alignment.md`](geo-h3-alignment.md).

---

## Summary Table

| Component | Phase 0 ready | Notes |
|-----------|---------------|-------|
| **bt_mon (BLE scanning)** | ✅ Yes | Works; raw payload and AD parsing still missing |
| **bt_iden (identity resolution)** | n/a — Phase 3 | Complete library, no consumer |
| **Database schema** | ⚠️ Partial | Authored; never executed; no partition for the current month |
| **Repository layer** | ✅ Yes | Models now match the schema; `h3o` used |
| **App (scan → sign → store)** | ⚠️ Partial | Path implemented in code; never run against a real DB or adapter |
| **Provenance / CA / revocation** | ⚠️ Partial | Signing done; verification, credential checking, and revocation unwired |
| **H3 geo-indexing** | ⚠️ Partial | Client derivation exists; unsigned in the payload |
| **MQTT / networking** | ❌ No | Phase 1 |
| **Gossip protocol** | ❌ No | Phase 5 |
| **HTTP API** | ❌ No | Absent from both the code and the dependency tree |

---

## Verification Snapshot (2026-09-02)

| Command | Result |
|---------|--------|
| `cargo test -p ca` | ✅ 27 passed |
| `cargo test -p bt_iden` | ✅ 41 passed |
| `cargo test -p bt_mon --lib` | ✅ 26 passed |
| `cargo test -p repo --lib` | ❌ 37 passed, 2 failed — `DATABASE_URL must be set` |
| `cargo test -p app --bins` | ❌ 196 passed, 1 failed — the racy `now == now_local` assertion |
| `cargo check --workspace --all-targets` | ❌ exit 101 — `bt_mon` mock example; 61 warnings, mostly dead code |

Dead-code warnings worth reading as signal rather than noise: `VerifyError::InvalidSignature`,
`VerifyError::InvalidPublicKey`, and `RevocationVerificationError::NodeRevoked` are *never
constructed* — three error paths that exist in the type system but that no code can reach,
which is precisely what an unwired verification workflow looks like.

---

## Environment Limits on Verification

This dev container has no Postgres server, no PostGIS, no h3-pg, and no Docker daemon.
Consequences:

- **No SQL in this project has ever been executed.** Schema correctness is argued from
  documentation, not observation.
- The two `revocation_repo` failures above are environmental, not defects.
- The `h3index` wire-behaviour probe (WS0 in `geo-h3-alignment.md`) must run on a host with a
  real database, and WS2/WS4 depend on its answer.
- Nothing in Phase 0's exit criteria — live capture volume, rate-limit threshold validation,
  24 h clock drift, "what did this node see in the last hour and where" — can be demonstrated
  from here.

---

## Resolved Since August 5, 2026

The questions this document previously listed as open have been decided and implemented:

| Former open question | Resolution |
|----------------------|------------|
| Canonical encoding for `signed_payload` | Canonical CBOR (RFC 8949) via `ciborium`; specified and implemented |
| `node_id` = UUID string vs SHA-256 of signing key | SHA-256 of the Ed25519 public key, `BYTEA`, implemented in `node/identity.rs` |
| Location: NUMERIC lat/lon vs PostGIS GEOGRAPHY | PostGIS `GEOGRAPHY(POINT,4326)`, `PostgisPoint` in the model |
| `device_hash`: TEXT hex vs raw bytes | Raw 32-byte `BYTEA` |
| `service_uuids` / `manufacturer_data`: JSONB vs typed | JSONB `signal_payload` column, unified across signal types |
| Ed25519 keypairs not implemented | Implemented, persisted, tested |
| Rate limiting not implemented | Implemented in `node/rate_limiter.rs` |
| GPS/location not implemented | Implemented via the `PositionSource` abstraction |
| `h3-pg` function signatures unverified | Verified against PGXN docs — **still unexercised at runtime** |
| Repo models do not match schema | Reconciled onto the unified `Occurrence` model |

---

## Critical Gaps Before Phase 0 Can Close

1. **Make the schema accept rows dated today.** Create the current-month partition and a
   provisioning plan before any insert can succeed.
2. **Run the migrations against a real Postgres + PostGIS + h3-pg** and settle WS0.
3. **Fix the two build/test defects** (mock example, racy clock assertion) so CI can be green.
4. **Wire one verification path end to end** — load a CA root, verify a peer's
   `ca_credential` once, cache it, and consult a *populated* RSL — so provenance produces an
   accept/reject decision rather than only a signature.
5. **Measure:** 1+ hour live capture, rate-limit threshold validated against real volume.

Still open from the original list: raw advertisement payload exposure, AD-type parsing, and
`nodes.status` / `last_seen_at` never being written.

---

## References

- **Gap analysis (deep, per component):** [`GAP_ANALYSIS.md`](../../GAP_ANALYSIS.md)
- **Architecture:** [`../architecture/overview.md`](../architecture/overview.md)
- **Schema analysis:** [`schema-divergence.md`](schema-divergence.md)
- **Repo analysis:** [`repo-divergence.md`](repo-divergence.md)
- **Provenance detail:** [`provenance-status.md`](provenance-status.md)
- **Geo/H3 detail:** [`geo-h3-alignment.md`](geo-h3-alignment.md)
- **Roadmap:** [`roadmap.md`](roadmap.md)
- **Blind spots:** [`../AGENTS.md#blind-spots`](../AGENTS.md)
