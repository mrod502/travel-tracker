# Advertisement payload: what `raw_payload` means, and what it may be scored for (TD-07 / D-A, M23, M28)

**Status:** decided by the owner on 2026-10-14 and implemented the same day. The fork
[GAP_ANALYSIS M28](../../GAP_ANALYSIS.md) recorded as "undocumented — no ADR exists anywhere under
`.knowledge/`" is this document. Nothing here is provisional: the code, the crate docs, and the
identity-adapter documentation all state the same contract, and the tests named below run without a
radio.

Measured against the tree on 2026-10-14 (`e565b7d` plus the working-tree pass described in
[`bt_mon/TECH_DEBT.md`](../../bt_mon/TECH_DEBT.md)), not against the field comment that preceded it.

---

## 1. The problem, stated as the arithmetic it is

`occurrences.signal_payload.ble.raw_payload_hex` exists, is gated by `store_raw_payload`, and is
inside the signed bytes ([B10](../../GAP_ANALYSIS.md)). `bt_iden` scores 35 of its 185 weight points
off AD structures — `field_layout`, `appearance`, `connectable`, and a manufacturer id read from the
framing rather than the decoded map ([B13](../../GAP_ANALYSIS.md)) — and measured coverage on stored
rows was **0.76 where the bytes were present and 0.32 where they were not**.

Neither real backend can produce those bytes. `btleplug::api::PeripheralProperties` carries parsed
fields only; bluer 0.17's `Device1` exposes `advertising_data` as a `HashMap<u8, Vec<u8>>` keyed by AD
type, and its own documentation says it contains "only types considered safe to be handled by
application". Both are parsers, not taps on the on-air buffer. So the field was documented as "the
bytes the backend reported, undecoded", `None` meant "not exposed", and both backends returned `None`
on real hardware forever — which made the flag, the column, and the 35 points live only on the
simulated radio.

The tempting fix — re-encode the parsed fields into length/type/value structures and hand them over as
though they had been captured — is the one that does real damage. A reconstruction is built from what
a parser chose to surface, so **every AD type the parser dropped, filtered, or did not understand is
absent from the reconstruction in exactly the way it would be absent if the device had not advertised
it.** Score that as an observation and the resolver is being told a device omitted a structure by the
person who chose the Bluetooth stack.

## 2. Options considered

| Option | What it means | Why it went |
|---|---|---|
| Leave it mock-only; record the gap | `None` on both real backends, permanently | Honest, and it was the state of the tree. But it leaves a BlueZ node storing nothing while the column, the flag and the signed key all imply otherwise, and it gives up the 35 points on the platform where BlueZ *did* parse the data |
| Canonical re-encoding, presented as payload | Build LTV structures from parsed fields, no labelling | Rejected on the argument in §1: it fabricates absence, and it does so in the one direction that inflates confidence — a device that looks like it advertised less is a device the resolver is likelier to keep separate |
| Drop to D-Bus / HCI for the real bytes | Read the advertisement buffer below these dependencies | Not this pass: it means owning a D-Bus subscription and an HCI path outside `btleplug`/`bluer`, on the one code path `app` does not currently use. It remains the only route to a *captured* payload on Linux, and it is still available |

**Decided:** a documented per-backend contract, with **provenance carried next to the bytes** rather
than assumed from the field's presence. Reconstruction is allowed only where it is labelled, and a
labelled payload's absences are not evidence.

## 3. The contract as implemented

`BluetoothDevice::raw_payload_source` (`bt_mon/src/types.rs`) carries `RawPayloadSource::{Captured,
Reconstructed}` alongside the bytes. `None` payload with `None` source is unchanged: this backend does
not expose advertisement bytes, which is never a statement about what the device advertised.

| Backend | `raw_payload` | `raw_payload_source` | What it is |
|---|---|---|---|
| btleplug | `None` | `None` | 0.12 exposes parsed properties only; there is nothing to hand over |
| bluer | structures, or `None` if BlueZ reported no advertisement properties at all | `Reconstructed` | LTV structures re-framed from `AdvertisingFlags`, `AdvertisingData`, `ManufacturerData16` and `ServiceData`, one structure per value, sorted by AD type |
| mock | the configured structures | `Captured` | `SimulatedAdvertiser::advertisement_bytes()`, built from what the test wrote |

Two limits come with the bluer row and are stated where the code is (`reconstruct_advertisement`) as
well as in `bt_mon/AGENTS.md` and the crate docs:

1. **Order is not on-air order.** BlueZ does not report the sequence the structures arrived in, so a
   reconstructed payload is sorted by AD type. Compare structures; offsets mean nothing.
2. **Absence is not evidence.** A missing type means BlueZ did not report it. `AdvertisingData` in
   particular carries only the types BlueZ considers safe to hand to an application, so the
   reconstruction is a floor on what was advertised and an upper bound on nothing.

Two details that are easy to get wrong and are pinned by tests rather than comments: a service-data
UUID arrives from BlueZ always as 128-bit, so the width that goes on air (16-, 32- or 128-bit, and
therefore AD type `0x16`/`0x20`/`0x21`) is recovered from whether the tail matches the Bluetooth Base
UUID, and the UUID bytes go on little-endian (`service_ad_uuid`); and a structure BlueZ already
reported through its own typed property is not written a second time from `AdvertisingData`.

## 4. What a consumer may do with it

`app/src/identity/adapt.rs` states the reading rule where the scoring is audited: present structures
are evidence, absences are not. `occurrences` has **no column for the source**, so a replay cannot tell
a captured row from a reconstructed one and must treat every stored payload as the weaker kind.

Closing that is three linked changes, none of them small enough to do incidentally:

- a migration adding the provenance to `signal_payload.ble` (or a column beside it);
- the writer taking it from `BluetoothDevice::raw_payload_source` rather than inferring it;
- a decision about the **signature**: `raw_payload_hex` is inside the signed bytes, so either
  provenance joins the signed shape — changing what a node attests to, and therefore the version — or
  it is stored outside and a verifier is told that the bytes it re-derives do not commit to how they
  were obtained. That choice belongs with the provenance work, not with this crate.

## 5. Evidence, and what it does not cover

Runs here, no radio: `a_reconstructed_payload_is_labelled_as_one`,
`a_payload_is_absent_when_bluez_reported_no_advertisement`,
`a_reconstructed_payload_orders_structures_by_type`,
`a_service_data_uuid_is_written_at_its_own_width_and_little_endian`,
`a_structure_already_reported_by_bluez_is_not_written_twice`, `the_name_is_not_in_the_payload`, and
on the mock side the `Captured` path through `advertisement_bytes`.

Not covered, and not coverable in this container: which advertisement properties a given BlueZ version
actually populates for a given peripheral — the reason the reconstruction is a floor — and whether the
reconstructed bytes for a real iBeacon/Eddystone match the captured ones from a second device. That is
hardware work; `cargo run --example bluer_scan_and_gatt --features bluer --no-default-features` is the
harness, and TD-04/TD-06/TD-07's status lines in `bt_mon/TECH_DEBT.md` carry the list.

The name is deliberately excluded from the payload even when BlueZ reports one: it can arrive from GATT
resolution rather than from the advertisement, and putting it in an AD framing would attribute a
resolved attribute to a packet that may never have carried it.
