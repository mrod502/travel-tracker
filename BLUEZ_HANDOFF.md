# Handoff: let the node select bluer, then prove it on a radio

Two parts, in order. **Part A** is code and finishes green in this container with no radio. **Part B**
cannot be done here — it needs a Linux box with BlueZ and a real peripheral, and it is the only thing
that can close the eight items `bt_mon/TECH_DEBT.md` records as waiting on hardware.

**Dispatch note: hand an agent Part A and stop.** Part B is a person with a dongle, a peripheral, and
`btmon` open; the most likely failure mode is an agent reporting it as done because the code reads plausibly.
If a single run must cover both, tell the agent explicitly that Part B is out of scope for this session.

## Why this is worth doing

`occurrences.signal_payload.ble.raw_payload_hex` is inside the signed payload, and `bt_iden` scores 35
of its 185 weight points off AD structures — measured coverage on stored rows was **0.76 where the
bytes were present, 0.32 where they were not**. btleplug never exposes those bytes and cannot. bluer
re-frames what BlueZ parsed and labels it `Reconstructed`. So on real hardware, `store_raw_payload`
produces anything **only on a bluer build** — and no build of the node can select bluer today. That is
the whole reason this task exists. D-B already decided: keep bluer, support GATT on Linux. The library
side landed 2026-10-14; the binary can't reach it.

## Ground rules

- **`bt_mon` is finished.** Do not "improve" it as part of this. A new defect you stumble on goes into
  `bt_mon/TECH_DEBT.md` as a numbered TD item, not into this change.
- Never commit, stage, or push — the owner handles git. Leave changes in the working tree and list paths.
- `export TMPDIR=/workspace/target/tmp` before any `cargo test`; cap link parallelism with `-j 2`.
- No CI files and no CI prose, anywhere.
- `gap_analysis/*.md` are dated snapshots — never edit them. `GAP_ANALYSIS.md` is the live tracker.
- **Nothing from Part B may be written up as verified unless a person ran it on that machine and the
  output was pasted in.** "The code says so" is exactly the failure mode this repo has been scrubbing.

---

# Part A — make bluer selectable (no radio required)

## A1. One knob — and the old one is deleted

The knob today is `use_mock_backend: bool`, present in all four layers (CLI `--use-mock-backend`,
`BT_USE_MOCK_BACKEND`, `[bluetooth].use_mock_backend`, `BluetoothConfig::use_mock_backend`) and consumed in
exactly one place, `App::run`. A bool can only say "mock or not-mock", so it cannot name bluer. The owner
decided on 2026-10-16: **delete it outright, no alias.** One selector, no precedence rule between two knobs
describing one fact — the same reasoning the sampling policy records as *"a second timeout able to
contradict `rate_limit_ms` is a second source of truth."*

- **Copy an existing setting rather than inventing one.** `[location].mode` is already exactly this shape —
  a string in the file, a hand-parsed string in the flag layer, an enum everywhere below it, and an error
  that blames whichever layer supplied the bad value. Follow it end to end:
  `LocationSection.mode: Option<PositionMode>` (`file.rs:91`, serde) → `parse_position_mode` (`layered.rs:592`)
  → `position_mode()` (`layered.rs:441`, flag > file > env > default) → `PositionMode`
  (`position/mod.rs:275`, `#[default]`, `Serialize + Deserialize`; see `GpsBackend` at `:288` for
  `#[serde(rename_all = "snake_case")]`). So: `enum BluetoothBackend { Btleplug, Bluer, Mock }` with
  `#[default] Btleplug` and `Display`, a `parse_backend(&str) -> Result<BluetoothBackend, String>` beside
  `parse_position_mode`, and a `backend()` resolver beside `position_mode()`. Flag stays `Option<String>` —
  that is how `--location-mode` does it, which is why its help shows no `[possible values]` list.
- Remove `use_mock_backend` everywhere: **40 references in `app`**, 3 in `bt_mon`'s doc comments, 13 in the
  docs listed in A6. Nothing in `.env.dev` or `.env.database` sets it and the node has never run outside a dev
  box, so there is no migration to write — and no deployment to break, which is why an alias buys nothing.
- Two dated records quote the flag inside their live-run evidence (`GAP_ANALYSIS.md` B14,
  `.knowledge/implementation/device-identity-decisions.md`). **Leave both as written** — they record what was
  run at the time.
- Because the key is deleted rather than renamed, **A1b is part of this change**, not an extra.

### A1b. A retired key left in a config file must not be silently ignored

`BluetoothSection` carries **no** `deny_unknown_fields` (contrast `LocationSection`, `file.rs:87-88`) and the
owner declined adding it on 2026-10-16, so the file layer stays tolerant of keys it does not know. That is
fine for keys from a future version — but it means a `config.toml` still containing `use_mock_backend = true`
will parse, drop the key without a word, and open **btleplug**. An operator asking for the simulated radio
would get a real radio, which is the exact silent inversion this repo has spent a pass removing.

Note the difference from the one precedent: `[bluetooth].fixed_location` deprecates *softly* because the field
still exists and warns at `layered.rs:476-478` (pinned by `a_deprecated_fixed_location_still_works_and_says_so`
at `:930`). A deleted field has nowhere to warn from — serde never sees it. So check the raw file at the load
site (`file.rs:139`, `toml::from_str`), before/alongside the typed parse: if `[bluetooth]` contains a
**named list of retired keys**, refuse startup naming each one and its replacement. Tolerant of unknown keys,
strict about the ones this project chose to retire.

Three different error shapes live on this path, so check which one you are in rather than assuming:

| Site | Shape |
|---|---|
| `file.rs:139` loader | `Result<_, String>` — `map_err(|e| format!("Failed to parse configuration: {e}"))`. A1b's check returns here |
| `layered.rs` `resolved(...)` | a **collected `Vec<String>`**, one message per broken setting, each naming the layer that supplied it (`layered.rs:1043-1055`: a bad flag blames `--location-mode`, a bad env value blames `BT_GPS_BACKEND`) |
| `validate_monitor` (`mod.rs:212`) | `Result<(), String>` — one message. A2 returns here |

Whatever the shape, the message names the setting, the value it got, and where it came from. Where a collected
list is reachable, prefer it: an operator fixing one mistake should not discover the next one an hour later.

Touch points, in the order that compiles:

| File | Change |
|---|---|
| `app/src/config/mod.rs:139` | `BluetoothConfig`: add `backend: BluetoothBackend`, **delete** `use_mock_backend:160` |
| `app/src/config/file.rs:67` `BluetoothSection` | add `backend: Option<BluetoothBackend>`, **delete** `use_mock_backend:78`; keep the section free of `deny_unknown_fields` and add A1b's retired-key check at the load site (`:139`) |
| `app/src/config/layered.rs:304-348` | resolve `backend` through the existing `self.text` shape, flag > file > env > default; add `BT_BACKEND` to `env_keys` (near `:53-59`); **delete** the `use_mock_backend` resolution at `:343-349` and `BT_USE_MOCK_BACKEND` from `env_keys` |
| `app/src/config/flags.rs:17` + `app/src/cli.rs:410,459` | add `--backend <NAME>`; `Option<String>` with **no** clap `default_value` (see the module doc in `flags.rs` — a defaulted flag outranks the config file); map it in `flags()`. **Delete** the `--use-mock-backend` flag (`cli.rs:408-416`) and its mapping (`:465`) |
| `app/src/config/mod.rs:212` `validate_monitor` | A2's availability check |
| `app/src/app.rs:155-195` | A3's single construction site |
| `app/src/node/full.rs:202-205, 251` | `FullNode`'s own settings struct carries `#[cfg(feature = "mock")] pub use_mock_backend: bool` and a `false` default — a **second** copy of the fact, one layer below the config. Hand it the resolved `backend` (or drop the field if `FullNode` no longer needs it) rather than replacing one bool with another |

## A2. A named backend that isn't compiled in must stop startup

This is the bug the docs just got scrubbed of, in code form. In `validate_monitor`, refuse a backend that
isn't available in this build, using the crate's own truth about the build —
`bt_mon::BackendKind::compiled()` (`bt_mon/src/error.rs:74`) — and have the error name the cargo feature and
list what *is* compiled. The mock case is the existing instance of it: `use_mock_backend = true` in a build
without the `mock` feature is silently ignored today and the node opens btleplug. With the bool deleted that
becomes one arm of the same check, so both backends get the behaviour for the price of one — which is the
actual argument for deletion rather than aliasing.

## A3. Construction: one site, feature-gated

Replace `app/src/app.rs`'s two `#[cfg]` blocks with a single `match` on `config.bluetooth.backend`, still
returning `Box<dyn bt_mon::DeviceMonitor + Send + Sync>` — **do not widen it to `GattClient`.** The node
uses `DeviceMonitor` only (verified: zero `subscribe`/`notifications`/`read_characteristic` call sites in
`app/src`), and `app/src/node/full.rs`'s test double implements `DeviceMonitor` alone. GATT in the node is
separate, later work.

Each branch must be `#[cfg(feature = "...")]`-gated, which means `app`'s feature table has to make a
bluer-only build expressible:

```toml
[features]
default = ["btleplug"]
btleplug = ["bt_mon/btleplug"]
bluer = ["bt_mon/bluer"]
mock = ["bt_mon/mock"]
```
and `bt_mon = { path = "../bt_mon", default-features = false }`.

That last line is what finally makes `--no-default-features --features bluer` mean what the deleted
documentation always claimed. Without it, bt_mon's own `default = ["btleplug"]` stays on and nothing is
 selectable. If a build ends up with no backend at all, that is a compile-time error (no branch reaches a
monitor) — leave it compile-time, not a runtime surprise.

## A3b. What Part A cannot verify — and what the report has to say about it

This container has **no adapter and no BlueZ D-Bus**, so the bluer branch and the btleplug branch are
*compile-verified only*, and no amount of `cargo test -p app` will change that. Three consequences:

- **Assert on the decision, not the device.** Test that the resolved `BluetoothBackend` is the right variant
  and that the selection function picks the right arm — not that a monitor came back. `MockMonitor` is the
  only monitor a test here can construct. If you find yourself calling `create_bluer_monitor_with_config` in
  a test, stop: that is Part B, and it will fail for reasons that have nothing to do with your change.
- **Deleting the bool breaks existing tests and fixtures by design** — `layered.rs:663, 862, 874-879`,
  `full.rs:251`, `cli.rs:1758-1789` all reference it. Rewrite them onto `backend`. A red test here is a
  to-do, not a regression, and re-adding the field to silence it is the one way to fail this task quietly.
- **Report wording:** "compiles; selection is unit-tested at the decision layer; neither real backend was
  executed, because there is no radio here." Do not write "bluer works" and do not write "all backends
  verified". Part B exists precisely because that claim has not been earned.

## A4. Tests — all runnable here

Name them in the existing prose-test style. At minimum:

1. `a_backend_setting_selects_the_backend` — `[bluetooth]\nbackend = "bluer"` resolves to `Bluer`.
2. precedence: flag beats file beats env beats default, using the `resolved(...)` harness in `layered.rs`.
3. `an_unavailable_backend_stops_startup_naming_the_feature` — in this container a default build asks for
   `bluer` and `validate_monitor` errors, naming `bluer` and listing what is compiled. Add the mirror arm:
   `backend = "mock"` in a build without the mock feature, which is A2 killing the pre-existing silent case.
4. `a_retired_mock_setting_in_the_file_is_refused_not_ignored` — A1b. `[bluetooth]\nuse_mock_backend = true`
   must be a load error naming `[bluetooth].backend`, not a config that quietly selects btleplug.
5. `the_mock_flag_no_longer_parses` — `--use-mock-backend` is an unrecognised argument (clap exit 2). Pin it so
   nobody later reads its absence as the flag having gone inert.
6. CLI: `--backend bluer` parses; `--backend blue` is refused by name; an unset flag leaves the layer below it
   intact (extend the tests near `app/src/cli.rs:1758`).
7. `the_mock_backend_still_gets_its_beacons` — `backend = "mock"` builds the same `MockConfig` the deleted
   branch built: advertiser count, advertisement interval from `scan_interval_ms`, adapter name from
   `adapter_id`. Deleting the bool must not change what the simulated radio *is*. This is also the natural
   place to let the crate do the mapping — `impl From<&MonitorConfig> for MockConfig` exists (TD-09) and
   `app/src/app.rs:159-169` still hand-rolls it.

## A5. Local gate — all of these, before Part B is even discussed

```bash
export TMPDIR=/workspace/target/tmp
cargo test -p app -j 2
cargo check -p app -j 2                                    # default (= btleplug)
cargo check -p app --no-default-features --features bluer   # bluer-only
cargo check -p app --features mock
cargo check -p app --features "bluer mock"
cargo clippy --workspace --all-targets -j 2                 # must stay ≤ 81 total, bt_mon == 0
cargo fmt --check -p app
grep -rn "use_mock_backend\|use-mock-backend\|USE_MOCK_BACKEND" \
  app/src bt_mon/src .config config.example.toml README.md app/README.md .qwen/FAST_PROMPT.md
# run after A6: must print nothing (the two dated records in A6 are excluded by path)
```

## A6. Then rewrite every document the change makes false

**(a) The ones that say this cannot be done:** `app/README.md` ("The `bluer` cargo feature is not a backend
switch"), the `NOT USABLE BY app YET` block in `.config/backends/bluer/config.example.toml`, and the closing
paragraph of `bt_mon/TECH_DEBT.md` ("The node cannot select bluer, and used to say it could") — mark that one
closed with the test names.

**(b) The 12 remaining references across 6 files.** Every command line in them changes shape:
`--use-mock-backend` → `--backend mock`, `BT_USE_MOCK_BACKEND=true` → `BT_BACKEND=mock`,
`use_mock_backend = true` → `backend = "mock"`. The root `README.md` key-configuration block,
`config.example.toml`'s `[bluetooth]`, `app/README.md` (3), `.config/backends/mock/config.example.toml` (4,
including its usage line), `app/src/main.rs:69`'s module doc, and `.qwen/FAST_PROMPT.md` (2 — the commands
block *and* the "no Bluetooth adapter" limit, which is the one future sessions here actually read).

**(c) bt_mon's three doc comments that name the app's flag** — `bt_mon/src/backends/mock.rs:145, 191, 420` all
describe `--use-mock-backend`. This is the one carve-out from "don't touch `bt_mon`": a library documenting a
flag that no longer exists is a doc defect, same class as TD-16. Change the wording only, no behaviour.

**Do not touch:** `GAP_ANALYSIS.md` B14 and `.knowledge/implementation/device-identity-decisions.md`, which
quote the flag inside dated live-run records.

Options go **before** the subcommand: `app --backend bluer monitor`, never `app monitor --backend bluer` (the
latter is a clap error, exit 2).

---

# Part B — the radio run

Eight items are open because no container test can reach them. Bring back, for each: the command, the
output, and the machine facts (kernel, `bluetoothd --version`, adapter chipset as `hciconfig -a` prints
it, `rustc --version`).

## B1. Before anything else

```bash
rfkill list ; bluetoothctl list ; hciconfig -a ; bluetoothd --version
getent group bluetooth        # is the scanning user a member? polkit may still refuse
```

`hciconfig` ships in a separate `bluez-deprecated` package on current distros — if it is missing, use
`bluetoothctl show` and `busctl --system call org.bluez /org/bluez/hci0 org.freedesktop.DBus.Properties
Get ss org.bluez.Adapter1 Address`, and say which you used. Absent tooling is not an absent adapter.

If `bluetoothctl list` shows no adapter, nothing below can run — stop and report that, don't infer.
Keep `journalctl -u bluetooth -f` and `btmon` open in other terminals for the whole session; half of
what's being checked is what BlueZ did, not what the node logged.

Have **two peripherals**: one stationary advertiser (iBeacon/Eddystone — the cadence tests need
something that does not move) and one connectable GATT device with a notify characteristic.

## B2. The harness

`cargo run --example bluer_scan_and_gatt --features bluer --no-default-features -- --adapter hci0 --scan 20`
(add `--device <ID>`, `--read <UUID>`, `--notify <UUID>`.) It prints the service table with
`is_primary`, and finishes with the `health()` counters — which is where most of the answers land.
Builds here; has never run anywhere.

| Item | Command / action | What closes it |
|---|---|---|
| TD-06 | stationary beacon, `--scan 60` | `Advertisement` keeps arriving at the scan cadence; events also fire when RSSI alone moves |
| TD-04 | same run, watch `manufacturer_data` / `service_data` | both populated for the beacon — BlueZ reporting them, not the code being able to |
| TD-07 | same run, payload + `btmon` | the `Reconstructed` payload's structures match what `btmon` captured for the same packet. **Absences will not match and must not be scored** — record which types differed |
| TD-03 | `--device <ID> --read <UUID>` | connect → discover → read succeeds on a *discovered* device (this was addressing the adapter's own MAC) |
| TD-08 | same, plus a disconnect | `services_resolved` true after discovery, false after the link drops |
| TD-02 | `--notify <UUID>`, then unsubscribe | frames arrive with `service` filled; `btmon` shows the CCCD written on subscribe and **restored** on unsubscribe |
| TD-01 | the same device on btleplug: `cargo run --example subscribe_notifications` | frames arrive there too — the same trait, the same stream shape, both backends. Run the two side by side and diff what each reports |
| TD-13 | `rfkill block bluetooth` mid-scan | `health().discovering` goes false within one scan interval and `last_error` is set; after `rfkill unblock`, note whether it recovers or needs a re-arm — write down which |
| TD-14 | 3 concurrent `device_events()` subscribers, then `busctl --system monitor \| grep Match` | one platform subscription regardless of subscriber count; and `dropped_events()` stays 0 with a fast consumer, rises (and logs) with a deliberately slow one |
| TD-01/06 duration | leave `--scan 1800` running | no lapse: `discovering` still true after 30 min; then `sudo systemctl restart bluetooth` and confirm `event_stream_opens` increments |

## B3. The node, end to end

Two database preconditions or every insert fails and it will look like the radio is broken:

```bash
cargo run --bin db -- --host database --port 5432 --user "$DB_USER" --db "$DB_NAME" up
# a partition covering the CURRENT month must exist (the migrations only make Jul/Aug 2026),
# and this node needs a row in `nodes` — `app ca ca-enroll` for its key — before it will scan.
```

```bash
cargo run --bin app --features bluer -- --backend bluer --location-mode fixed \
  --fixed-location "40.6892,-74.0445" --store-raw-payload monitor
```

Then, in the database: rows whose `signal_payload.ble.raw_payload_hex` walks as real AD structures
(`0x01` flags, `0x16`/`0x20`/`0x21` service data, `0xff` manufacturer), a record rate set by
`rate_limit_ms` and not by the radio, and — the payoff — **non-null payload where the same beacon on
btleplug stores nothing.**

## B4. Where the results go

- `bt_mon/TECH_DEBT.md`: the `Status 2026-10-14` column entries, each item's `**Status — …**` paragraph,
  and the list in "What this pass closed, and what it left for a radio". Close what the run proved; for
  what it disproved, write a **new** TD item with the pasted output rather than editing the old claim.
- `GAP_ANALYSIS.md` rows B10, B14, M23 (live tracker only, never `gap_analysis/`).
- The backend matrix in `bt_mon/AGENTS.md` — its rows currently say "established by reading the code,
  not by running it". Delete that qualifier for whatever was actually observed.
- `.knowledge/implementation/status.md`: a dated run block (machine, BlueZ version, per-item result).

## Done when

Part A: the five `cargo check` variants compile, the seven A4 tests pass, clippy is ≤ 81 with `bt_mon` 0,
A5's grep for the retired setting prints nothing, and no document A6 lists still contradicts the binary.
Part B: each of the ten rows in B2 has pasted output attached, TD-07's payload comparison is recorded
including what differed, and every claim left open says so in the present tense.
