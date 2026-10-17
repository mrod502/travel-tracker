# Device identity — keying (B15) and the platform-identifier constraint (M36)

**Status:** B15 **deferred by the owner on 2026-10-13**; M36 **re-scoped the same day** —
the constraint is backend-specific, not platform-absolute, and the remaining work is a
TODO ([M38](../../GAP_ANALYSIS.md)) rather than a fact about Apple hardware.

Nothing in this document has been implemented as a change to what a node hashes. It records
the state of the decision, the evidence it rests on, and — because a deferred decision is
the kind that gets silently reversed — the tests that make reversing it loud.

**What changed on 2026-10-13:** [M37](../../GAP_ANALYSIS.md) is fixed, so a Linux node now
hashes a real MAC instead of a string containing its own adapter name. That is a precondition
for B15 being worth deciding at all: before the fix there was no address arriving to key.

---

## 1. Where the scheme actually stands

Measured against the tree on 2026-10-13, not against the register's description of it:

| Claim | Where | Reality |
|---|---|---|
| `device_hash` for a MAC is `SHA-256(mac_bytes)`, untagged, no key | `app/src/node/device_id.rs:107` | Yes — `hash: sha256(&address)` |
| Non-MAC identifiers are tagged before hashing | `device_id.rs:117,124` | Yes — `ble:id:uuid:v1:`, `ble:id:opaque:v1:` |
| The MAC path deliberately stays untagged | `device_id.rs:40-44` | Yes — "to stay compatible with occurrences written before this module existed" |
| The column is documented as pseudonymous | `db/src/migrations/202607312147_create_bluetooth_occurrences.sql:40` | Yes — `-- pseudonymous id (32-byte SHA-256 hash)` |
| The same row also stores the raw address | same file `:39`, `-- raw MAC/address` | Yes — `device_address BYTEA` |
| `device_hash` is inside the signed bytes | `app/src/provenance/payload.rs` v2 element 3, `bytes(32)` | Yes — so is `device_address`, element 4 |

So the row that is described as holding a pseudonym holds, in the next column, the thing the
pseudonym is a digest of. That is the fact every B15 option has to reckon with, and it is why
"add a key" is not the two-line change it looks like.

## 2. The enumeration argument, with the arithmetic shown

The register says the space is covered "in hours". Written out: the Bluetooth address space is
48 bits, `2^48 = 281 474 976 710 656` candidates, each one hash — so the preimage work per
candidate is one SHA-256 compression over a single 6-byte block. At an assumed
`10^11` compression functions/second — a figure assumed here, **not measured on any hardware in
this container, which has no GPU** — covering the space is `2 815` s, under 47 minutes; a
hundred-fold faster device does it in under 30 seconds.

The arithmetic is not the interesting part. Two things about it are:

1. **A precomputed table is not needed to do the damage.** The targeted question — *is this
   known device in this table?* — is one hash and one index lookup. `idx_occurrence_device_hash`
   (`db/src/migrations/202608020206_create_occurrence_indexes.sql:3`, on
   `(device_hash, observed_at DESC)`) exists precisely to make lookups by that value cheap. The
   enumeration cost is paid by whoever
   already has a device in mind, and they always will: a phone, a car key fob, a tracker.
2. **The row already contains the answer.** Hashing the MAC while storing the MAC protects the
   hash from people who can read the hash and *not* the row. Whether such a reader exists is a
   question about the deployment, and no deployment exists yet — federation is
   [B16](../../GAP_ANALYSIS.md) and is at zero.

Point 2 is the honest reason the decision is not urgent, and point 1 is the honest reason it is
not free to postpone forever: the schema comment calls the column pseudonymous, and it is not.

## 3. Alternatives considered

Costed against what the tree does today, not in the abstract.

**A. HMAC-SHA-256 with a shared realm key, from a file.**
`device_hash = HMAC-SHA-256(realm_key, tagged_identifier)`. Every node holding the key computes
one comparable hash, so cross-node identity and `device_address_links` keep working as a lookup.
A row records which key produced its hash. Cost: key distribution becomes a real operational
artefact, and rotation stops every cross-rotation join. Needs `hmac` as a direct `app`
dependency (`Cargo.lock` already carries `hmac 0.13.0` and `hkdf 0.13.0` via `sqlx-postgres`, but
`app` pins `sha2 0.10` while `hmac 0.13` needs `digest 0.11`, so `app`'s `sha2` moves to `0.11`
too — a dependency change, which is why it is named here rather than just done).

**B. Per-node key (a node-local pseudonym).**
No global correlation key exists anywhere, which is the strongest privacy property available. It
also deletes the premise of the `device_hash` index: two nodes seeing one peripheral would report
different hashes, so "same device, seen elsewhere" stops being a lookup and becomes the
resolver's fingerprint problem, permanently. Chosen only if the answer to §5 is "cross-node
comparability is not wanted".

**C. Keep SHA-256; stop storing the raw address.**
`device_address_links` already has an `address BYTEA` column with the 6-byte CHECK, so the address
has somewhere else to live. This fixes the row's privacy — a reader of `occurrences` would hold a
hash and nothing else — and leaves the hash itself enumerable. Cheaper than A on key management,
worse on the enumeration point, and it changes what signed element 4 means.

**D. No key; relabel.**
Rename the column comment and any prose that calls `device_hash` pseudonymous, and stop making a
claim the scheme does not support. Zero migration, zero key management, zero privacy gain. Cheap
enough that it should happen whatever else is decided, because today's documentation is wrong
rather than merely weak.

**Not considered:** salting per row (a per-row salt in the row makes the digest a plain function
of salt plus address, so it hides nothing), and encrypting the address without a hash (there is
then no cheap equality lookup, and the index that exists is the equality lookup).

## 4. Decision: deferred (2026-10-13)

The owner's decision, verbatim: *"the MAC address should stay in for now. I want to do some more
research into the MacOS landscape and see if there is any way to get around this. So the explicit
decision here is defer: keep the same for now."* Rotation was raised and set aside as not
relevant while the deferral stands.

The deferral is sound reasoning, and it is worth stating why, because "we did nothing" and "we
decided to wait for one specific piece of information" are different documents:

- macOS may not have the constraint this project assumed (§5). If a macOS node can report a real
  address, then every node in the fleet is a MAC-reporting node, and the choice between A and B
  becomes a privacy question about federation rather than a compatibility question about Apple.
- Keying now, before anyone knows who is ever permitted to read a row (§2, point 2), would mean
  choosing a key-distribution model for a federation that does not exist.
- The cost of waiting is bounded and mostly documentation: the enumeration argument is true today
  and stays true; it does not get worse per row stored. What does get worse per row is *option
  C* — every new row makes "stop storing the raw address" a bigger backfill. That is the real
  time pressure here, and it is on C, not on A.

**What "deferred" does not mean.** It does not mean the schema comment keeps saying
"pseudonymous". Until B15 lands, that word is inaccurate, and the accurate statement is the one
in §2.

### The guard against a silent reversal

A deferral that lives only in a document is reversible in a refactor. Two tests make it
not:

- `app/src/node/device_id.rs::the_unkeyed_scheme_is_pinned_by_its_own_bytes` pins the exact output
  bytes of all three derivation branches, including the pre-M37 opaque spelling so old rows stay
  reproducible. Any key moves every vector.
- `app/src/node/full.rs::a_stored_signature_over_a_mac_derived_device_hash_verifies` signs a row,
  reads it back, and verifies the stored signature over the stored bytes, then asserts the payload's
  element 3 and 4 equal the row's columns. It is the test that has to keep passing across a B15
  change — and it is written so that keying the hash fails something with `device_hash` in its name
  rather than showing up as rows that stop verifying in production.

### Design recorded for whoever decides it

Not implemented, so it cannot drift into being half-implemented:

- Configuration follows `[revocation]` (`config.example.toml:167-187`,
  `AppConfig::validate_revocation` at `app/src/config/mod.rs:247`, and the runtime half
  `RevocationWatch::start` at `app/src/node/revocation.rs:114`): an `[identity]` section with
  `key_path`, an env override, and a missing-or-unreadable key while enabled being a **startup
  error naming both ways out** — supply the file, or turn the feature off. Never a silent fall
  back to the unkeyed scheme, because that produces a fleet where some rows are keyed and nobody
  knows which.
- The key comes from a file path. Never the database — a reader of the database must not be able
  to undo the hashes — and never source, per the project rule against committed key material.
- A row records the id of the key that produced its hash, and the scheme tag
  (`sha256-unsalted-v1` vs the keyed name) that makes old rows and new rows distinguishable.
  Without a per-row tag, the boundary between the two schemes is a timestamp, and a timestamp is
  only evidence if you trust the clock that wrote it.
- Rotation by file replacement, with the key id per row, so no backfill is required
  (`occurrences` is append-only and is never rewritten).

## 5. What happens to rows already written

Nothing, and that is the property worth naming:

- Every row written under the unsalted scheme keeps its `device_hash` bytes. The scheme is not
  recorded in the row, so old rows are indistinguishable from new ones **by content** — only by
  the deployment's history. This is the gap the per-row tag in §4 exists to close, and it is the
  strongest argument for adding the tag *before* the keyed switch rather than during it.
- Old signatures still verify. `device_hash` is signed as opaque `bytes(32)`; the verifier checks
  the bytes that were stored, and does not re-derive them from the identifier. Nothing in a future
  keying change reaches back into a stored signature. Verified here by
  `a_stored_signature_over_a_mac_derived_device_hash_verifies`.
- **What identity queries can no longer assume after a keyed cutover** — the list §4's tag is
  needed to make enforceable rather than remembered:
  1. *One device, one `device_hash` across a rotation boundary.* It stops being true. The join
     becomes `device_address_links` → `device_identities`, keyed by fingerprint, and that join is
     an inference with a confidence score, not an equality.
  2. *The same device reported by two nodes has the same `device_hash`.* Only true if both nodes
     hold the same key. A node that rotated ahead, or that never received the key, reports a
     different identifier for the same peripheral, and nothing in the row says which happened
     without the tag.
  3. *`device_hash` is stable across a re-import of the same capture.* Re-keying between two
     imports of one dataset makes the second import look like a set of new devices, with no error.
  4. *`idx_occurrence_device_hash` finds every row for a device.* After a rotation it finds the
     rows from one key period only.

## 6. M36 — the platform-identifier constraint

### What is true, and what was assumed

Re-scoped on 2026-10-13 after the owner reported obtaining MAC addresses on macOS through
`system_profiler`. That report is **not verified in this document** — this container is Linux and
has no macOS to check it against — but it is enough to withdraw the absolute claim M36 originally
made, which was that a macOS node has no address to store.

The two halves, separated:

- **What btleplug gives a macOS node today is a CoreBluetooth peripheral identifier, not an
  address.** That is a property of the API btleplug uses. It is recorded in the tree, not argued:
  `app/src/node/device_id.rs:2-8` and `bt_mon/src/types.rs:17-29`, and the branch itself takes any
  unparseable identifier to `IdentifierSource::Opaque` (`device_id.rs:120-128`) or a UUID to
  `Uuid` (`:113-119`). The 2,140-row macOS capture of 2026-10-12 carries
  `signal_payload.ble.id_source = "uuid"` in every row and NULL `device_address` in every row
  ([M36](../../GAP_ANALYSIS.md)).
- **Whether macOS can give a node an address at all is open.** `system_profiler` is a system
  utility reading the system's own Bluetooth state; it is not the API path a third-party central
  uses. What has to be established before M38 is scheduled is which API the observed address comes
  from, whether a headless node can read it, whether it covers *advertising* peripherals or only
  paired/known ones, and what entitlements or sandbox rules attach.

### The consequence that stands either way

A `uuid` identifier is assigned by the host that saw the device. One physical peripheral observed
by two macOS hosts produces two unrelated identifiers, and — because `adapt` keys a
no-address observation on six bytes of `device_hash` (`app/src/identity/adapt.rs:284-286`,
`six_bytes_of` at `:325`) — two unrelated six-byte "addresses" that the resolver compares with the
same decisive exact-address feature it uses for a MAC.

**For federation.** [B16](../../GAP_ANALYSIS.md) is the point at which a node accepts another
node's rows. Joining them on `device_hash` is sound for `ble_mac` rows and **unsound for `uuid`
and `opaque_id` rows**: the same peripheral arrives as two hashes (it fails to join, which is
merely lost information), and two different peripherals can coincide on six of 48 bits, which is
a false merge silently asserted across a trust boundary. Federation must therefore treat
`identifier_source` as part of the join key, not as metadata beside it. `observer_node_id` is
already scoped that way in `device_address_links`
(`db/src/migrations/202609271430_create_device_identity_tables.sql`, the column comment on
`observer_node_id`); the same rule has to be restated where cross-node joining first gets
implemented.

### What was implemented on 2026-10-13

Reporting, not enforcement. No resolver threshold changed and no merge outcome moved, so any
comparison against an earlier replay report still means what it meant.

- Each decision now carries `non_comparable_join` (`app/src/identity/replay.rs:156`), set when an
  observation is absorbed into an identity already held by a different identifier source
  (`spans_other_source`, `:735`). The decision line gains a `NOT COMPARABLE` annotation and the
  report gains a summary count (`non_comparable_joins`, `:289`) reported separately from refusals,
  because these were *not* refused.
- The report header states the identity scheme (`identity_scheme`, `:304`): `sha256-unsalted-v1`.
  One value today, because there is only one scheme today; the point is that the day there are
  two, a report line already says which one it belongs to instead of a reader having to infer it
  from the date of the run.
- The persisted fingerprint already keyed on identifier source and collapses a mixed identity to
  `mixed` (the rule at `app/src/identity/fingerprint.rs:24-28`, the field at `:57-59`, the collapse
  at `app/src/identity/replay.rs:775`), so a
  mixed-source identity never shares a `fingerprint_hash` with a clean one. That predates this
  work; what is new is that the *decision* says so at the moment it is made, not only in the row
  written afterwards.
- Pinned by `a_decision_across_identifier_sources_is_reported_as_non_comparable`, with
  `two_observations_under_the_same_source_are_not_flagged` as the control that keeps the flag from
  firing on every merge.

**Explicitly not done:** refusing cross-source merges. That would change merge outcomes on real
rows, and would make it permanently impossible to join a device a node first knew by UUID with the
same device once it has a real address — which is exactly the transition M38 is expected to create.

## 7. Follow-up recorded 2026-10-13

**M38 (Major, not Blocking): a macOS backend that reports MAC addresses.**

If a macOS node can obtain a real peripheral address, the whole `uuid` branch of this analysis
shrinks to a compatibility concern for old rows: cross-node comparability becomes available on
every supported platform, `device_address_links` becomes feedable from macOS for the first time,
and B15's A-vs-B choice stops being constrained by Apple at all. Major rather than Blocking
because Linux is expected to be the first choice for nodes, and nothing today is *wrong* on macOS
— it is narrower than it could be, and it says so.

To establish before scheduling: which API the `system_profiler` address comes from; whether a
headless node can read it without a GUI session; whether it is available for peripherals that are
advertising but not paired; what entitlement or sandbox exception attaches; and what it costs in
power and latency relative to a passive scan.

## 8. Re-measured numbers

The 0.36 mean evidence coverage quoted in B12/B13 was measured on 2026-10-08 over a window of
five mock beacons captured in **two different qualities** — rows written with AD bytes and rows
written without them, which is why it was low.

Re-measured on 2026-10-13 over a fresh scratch database (`m37_scratch`, created and dropped in
this session; `travel` untouched): 56 occurrences, 5 identities, 51 merges, **mean coverage 0.76**,
four refusals (all `contradicted`, the name veto working), and `--write` recording 5 identities,
5 links, 10 co-presence events, 10 association edges — 5 of 5 links with a 6-byte address and
`method = exact_address`, `fingerprint->>'identifier_source' = 'ble_mac'` on all 5.

**0.76 is not an improvement over 0.36 and must not be reported as one.** Every row in the new
window carries `raw_payload_hex` (56 of 56), so layout, appearance and payload similarity all had
a source; the 0.36 window deliberately mixed in rows that had none. What the new number
establishes is the pipeline, not a trend: `identity-replay` runs against stored rows, and
`device_address_links` — which M36 records as unfeedable from macOS and M37 records as unfeedable
from Linux — now receives a 6-byte address written by the `app … monitor` CLI path rather than by a
test harness. (The mock backend supplied the advertisements; the row-writing path is the real one.)

Both captures are mock-backend, not radio: this container has no Bluetooth adapter —
`/sys/class/bluetooth` does not exist, and `/run/dbus` is an empty directory, so there is no
`system_bus_socket` and `bluetoothctl list` dies on a NULL D-Bus connection — and the mock path
never goes through `peripheral_id_to_device_id`, so no run here can demonstrate M37's effect on
hardware. The BlueZ path is pinned by unit tests instead
(`the_bluez_peripheral_id_becomes_a_bare_mac`, `a_peripheral_heard_through_bluez_is_stored_with_its_address`).

## 9. Reproducing the measurements

```bash
export TMPDIR=/workspace/target/tmp                      # /tmp is noexec here
export PGPASSWORD=devpassword

psql -h database -U postgres -c 'CREATE DATABASE m37_scratch;'
cargo run -q -j 2 -p db -- --host database --user postgres --db m37_scratch up

# throwaway CA + enrollment, both under target/tmp (nothing committed, nothing shared)
cargo run -q -j 2 -p app -- --data-dir target/tmp/m37-node ca ca-init \
    --key-path=target/tmp/m37-ca/root_key.pem
cargo run -q -j 2 -p app --features mock -- \
    -d 'postgres://postgres:devpassword@database:5432/m37_scratch' \
    --data-dir target/tmp/m37-node ca ca-enroll \
    --public-key-file target/tmp/m37-node/node_identity.pub.pem \
    --key-path target/tmp/m37-ca/root_key.pem --lat=40.7128 --lon=-73.91

# a short capture (no duration flag; bound it with timeout)
timeout 45 cargo run -q -j 2 -p app --features mock -- \
    -d 'postgres://postgres:devpassword@database:5432/m37_scratch' \
    --use-mock-backend --location-mode fixed --fixed-location "40.7128,-73.91" \
    --data-dir target/tmp/m37-node --rate-limit-ms 4000 --scan-interval-ms 500 monitor

cargo run -q -j 2 -p app -- -d 'postgres://…@database:5432/m37_scratch' \
    --log-level error identity-replay --last 24h          # read-only
cargo run -q -j 2 -p app -- -d 'postgres://…@database:5432/m37_scratch' \
    --log-level error identity-replay --last 24h --write  # writes the four derived tables

psql -h database -U postgres -d m37_scratch -c \
  "select method, count(*), count(address) from device_address_links group by 1;"

psql -h database -U postgres -c 'DROP DATABASE m37_scratch;'
```

`--lon=-73.91` needs the `=` form: a bare negative value is parsed as a flag.
