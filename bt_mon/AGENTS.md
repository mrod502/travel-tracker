# bt_mon — backend-agnostic Bluetooth LE monitoring

A thin async abstraction over one Bluetooth radio: scan, discover, connect, do GATT I/O, and turn
what the radio heard into a stream of `DeviceEvent`s. It is a *radio* layer only — no persistence,
no rate limiting, no identity resolution. Consumers (`app`) store occurrences and decide what a
sighting is worth; `bt_iden` resolves identities from stored observations and does **not** depend
on this crate.

It is a **central**: it scans and talks to peripherals. There is no GATT server and no advertiser
here, on any backend — bluer included.

## Public surface

Two traits, one object: `GattClient: DeviceMonitor`, both `Send + Sync`, all methods take `&self`.

- `DeviceMonitor`: `start_scan`, `stop_scan`, `devices`, `device`, `is_powered`, `adapter_info`,
  `device_events`, `is_scanning`, and `health` (defaulted to `None` so an implementor outside this
  crate — `app`'s test doubles — need not answer; all three backends here do).
- `GattClient`: `connect`, `disconnect`, `is_connected`, `discover_services`, `services`,
  `read_characteristic`, `write_characteristic(_, _, value, response)`, `subscribe`, `unsubscribe`,
  `notifications`.
- Types: `DeviceId`, `ServiceUuid`, `CharacteristicUuid`, `BluetoothDevice`, `GattService`,
  `GattCharacteristic`, `CharacteristicProperties`, `ValueNotification`, `DeviceEvent`,
  `UpdateField`, `NotificationEvent`, `RawPayloadSource`, `MonitorHealth`, `Error`/`Result`,
  `MonitorConfig`, `adapter_matches`, `DEFAULT_SCAN_INTERVAL` (1 s).
- Pure helpers a consumer may reuse: `report_event(old, reported)` and
  `changed_device_fields(old, new)` — the same functions the backends use to decide what a report
  means.
- Entry points (each feature-gated): `create_btleplug_monitor()` /
  `create_btleplug_monitor_with_config(MonitorConfig)`, `create_bluer_monitor()` /
  `create_bluer_monitor_with_config(MonitorConfig)`, `create_mock_monitor()` /
  `create_mock_monitor_with_config(MockConfig)`. The `_with_config` variants return the concrete
  struct; `create_btleplug_monitor()` and `create_bluer_monitor()` return `impl GattClient`.
  `MockMonitor::new()` / `with_config()` are sync and expose the harness API (`add_device`,
  `add_device_with_services`, `set_device_services`, `remove_device`, `clear_devices`, `advertise`,
  `send_notification`).

Features: `default = ["btleplug"]`, plus `bluer`, `mock`, `full`. Every backend module, its entry
point, and each example (`required-features`) are gated. `cargo check -p bt_mon
--no-default-features` compiles: types and traits only, no monitor. `app/Cargo.toml` pulls the
default and forwards `bluer`/`mock`.

## Use it like this

```rust
use bt_mon::{create_btleplug_monitor_with_config, DeviceMonitor, MonitorConfig};
use futures::StreamExt;
use std::time::Duration;

let monitor = create_btleplug_monitor_with_config(
    MonitorConfig::new().with_adapter("hci1").with_scan_interval(Duration::from_millis(500)),
).await?;

// Open the stream *before* scanning so nothing published at startup is missed.
let mut events = monitor.device_events().await?;
monitor.start_scan().await?;

while let Some(event) = events.next().await {
    // DeviceAdded | DeviceUpdated { changed_fields } | Advertisement | DeviceRemoved
}
```

Scanning is continuous, not a duty cycle: `MonitorConfig::scan_interval` is how often the monitor
re-arms the radio (btleplug: stop+start, `btleplug.rs:241`; bluer: tear down and re-open BlueZ
discovery) so a session that lapsed without saying so — adapter sleep, `bluetoothd` restart —
cannot leave a running node deaf. `Duration::ZERO` disables re-arming. Both real `start_scan` calls
block ~500 ms and **clear the device cache**.

Adapter selection (`adapter_matches`): case-insensitive, whitespace-trimmed equality with the whole
description or its first field, plus substring only for a selector containing `:`. Prefix matching
is excluded on purpose — `hci` must not choose between `hci0` and `hci1`; an ambiguous or unmatched
selector is an error naming what the system actually has.

## Event-model invariants (the load-bearing part)

- `report_event(old, reported)` (`monitor/events.rs:136`) is the only thing that decides what a
  report means: `DeviceUpdated` with a **non-empty** `changed_fields`, else `Advertisement`.
- No report is ever swallowed. A stationary beacon — same name, same RSSI, same payload — keeps
  producing `Advertisement`. "No event" is indistinguishable from "the device left" (GAP_ANALYSIS
  B14); whether a sighting deserves a row is the consumer's policy, not the backend's.
- `changed_device_fields` (`monitor/events.rs:104`) returns fields in a fixed order
  (Name, Rssi, ServicesResolved, Connected, ManufacturerData, ServiceData, RawPayload) — declaration
  order, not the order fields were touched, so consumers may compare the vector directly.
- `raw_payload_source` is deliberately **not** a tracked field: a payload that changes provenance
  without changing bytes is the same observation, and a `Reconstructed` payload must never be
  compared byte-for-byte against a `Captured` one as though they were the same kind of evidence.
- `id`/`address` are identity, not tracked fields: a report whose address differs is another device.
- The advertised *service list* has no field on `BluetoothDevice`, so `ServicesAdvertisement` and
  `DeviceServicesModified` cannot move a tracked field; both are delivered as `Advertisement`.
- Every consumer of one monitor shares one platform subscription (`DeviceEventSinks`,
  `monitor/events.rs:200`); the pump parks while nobody is subscribed rather than pulling reports
  into a void. `device_events()` on bluer and the mock replays the current cache to a late
  subscriber; btleplug does not.
- Channels are lossy: capacity `DEVICE_EVENT_CHANNEL_CAPACITY` = 256, and the pump `try_send`s and
  **drops** events rather than block behind a slow consumer. Drops are counted, not silent:
  `BtleplugMonitor::dropped_events()` (first drop and then every 256th logs),
  `NotificationHub::dropped_count(device)`.
- Duplicates are normal: BlueZ synthesizes discoveries when the btleplug stream opens, and the mock
  replays its current devices to every new subscriber.

## What each backend can tell you

| | btleplug (default) | bluer | mock |
|---|---|---|---|
| `device_events()` | real (`CentralEvent` pump, shared by all subscribers, reopened when the source ends) | real (BlueZ discovery stream, re-armed every `scan_interval`, re-opened on `bluetoothd` restart) | real (broadcast + replay to each new subscriber) |
| `notifications()` | real: one reader per device over `Peripheral::notifications()` | real: one BlueZ notify session **per `(device, characteristic)`**, fanned into the same per-device stream | real: `send_notification`, or `auto_send_notifications` after a `subscribe` |
| `manufacturer_data` / `service_data` | populated | populated from BlueZ's typed properties | only what `advertise()`/config puts there |
| `raw_payload` | always `None`, `raw_payload_source: None` | re-framed LTV structures, `raw_payload_source: Reconstructed` | the bytes configured, `Captured` |
| `services_resolved` | tracked by this crate: set by `discover_services`, cleared when the link drops (btleplug 0.12 exposes no such query) | BlueZ's own property, plus the transition `discover_services` produces | stays `false` |
| `GattService::is_primary` | real | BlueZ's answer | as configured |
| `write_characteristic(..., response)` | honoured (`WriteType`) | honoured (`WriteOp::Request` / `Command`) | ignored |
| GATT ops on a discovered device | work | work — the cache holds the `bluer::Device` handle, not an address looked up later | work |
| `unsubscribe` | real | ends that characteristic's BlueZ session, which is what restores the CCCD | clears the flag, stops auto-send |
| scan cadence | `scan_interval` | `scan_interval` | `MockConfig::advertise_interval_ms` |
| `is_scanning()` | requested **and** the radio is believed to be delivering | same | same, from `adapter_powered` |

Reading the payload row: bluer's payload is built from the advertisement properties BlueZ parses
and sorted by AD type, with service data written at the width its UUID actually has (16-, 32- or
128-bit, little-endian on air, recovered from the Bluetooth Base UUID because BlueZ always reports
the key as 128-bit) and a type BlueZ already reported not written twice. Two consequences: byte
offsets mean nothing, and an absent AD type means "BlueZ did not report it", never "the device did
not advertise it". `backends/bluer.rs`'s `reconstruct_advertisement` carries the same limits at the
code.

`adapter_info()` formats differ per backend and are not selectors. The mock's
(`"<name> (mock) powered=…"`) satisfies `adapter_matches`; bluer's (`"Adapter 'hci0' (…)"`) does
**not** — bluer matches internally on `"name (address)"`, so don't round-trip `adapter_info()`
through a selector there.

Cross-cutting:

- Errors are mapped per backend through `map_gatt_error` (`btleplug.rs:453`, `bluer.rs:697`), so a
  dropped link arrives as `Error::Disconnected`, an refused or failed operation as
  `Error::GattError { operation, message, .. }` with the stack's own words in `message`, and
  "this build has no such backend" as `Error::BackendUnavailable { required, available }`
  (`BackendKind::compiled()` builds the `available` list). `error.rs`'s module doc is a table of who
  raises what — read it before writing a handler.
- Ordering matters on btleplug: `read_characteristic`/`subscribe` look the characteristic up in
  `peripheral.characteristics()`, which `discover_services()` populates — skip it and you get
  `CharacteristicNotFound`, not a protocol error. All GATT ops on every backend return `NotConnected`
  unless the device is connected.
- A notification stream is per **device**, not per characteristic: `notifications(id)` carries every
  frame that device sends, including frames another consumer subscribed for, which is why
  `ValueNotification::service` is filled (btleplug from the frame's service uuid, bluer and the mock
  from the service the subscription was opened through).
- Health is not an event. `MonitorHealth` (`monitor/health.rs`) is how a consumer distinguishes an
  empty room from a dead radio: `discovering` goes false within one scan interval of the adapter
  going off or every re-arm failing, `event_stream_opens` counts reopens after `bluetoothd`
  restarts, and `last_error` is sticky so a poller that checks every minute still learns something
  happened. A backend that measures nothing returns `None` rather than zeroes.
- Mock specifics: `start_scan` does **not** clear devices (the real backends do); `read_characteristic`
  returns the stored value and fails `InvalidArgument` when `allow_read` **or** `properties.read` is
  false, and `subscribe` likewise for `allow_subscribe`/`notifies()` — the two gates are independent
  and both apply; injected failures surface as `BackendError` (scan, connect) and `GattError`
  (read, write); a `stop_scan` → `start_scan` pair retires the previous advertiser loop via a
  generation counter, so the event rate does not double.
- Only the mock is exercisable in this container (no Bluetooth hardware; bluer needs BlueZ/D-Bus).
  The btleplug and bluer runtime paths are established by reading the code, not by running it; the
  hardware-pending list is at the bottom of `TECH_DEBT.md`.

## Best practices

- Build ids with `DeviceId::from_platform_id`; never hash a raw platform id. BlueZ reports
  `hci0/dev_F4_FE_FB_CC_78_58`, and leaving `hci0` inside the identifier makes one peripheral two
  devices on a node with a second dongle, or after a reboot that renumbers adapters (GAP_ANALYSIS
  M37). `DeviceId::new` is for ids that are already canonical.
- Treat `raw_payload: None` as "this backend does not expose the bytes" (btleplug always, bluer when
  BlueZ reported no advertisement properties at all), never as "the device advertised nothing".
- Drive off `device_events()` on all three backends. On btleplug, after `start_scan`'s initial
  snapshot the cache only advances through the pump; on bluer the discovery stream is the only
  channel and it also carries the property-change re-emissions that keep the cache current.
- Keep the event consumer fast and non-blocking; slow consumers lose events by design. Counting
  them is available; not losing them is not.
- Don't infer "device gone" from silence, and don't treat `Advertisement` as a change. Check
  `health()` before concluding the room is empty.
- Adding a backend or a `BluetoothDevice` field: implement both traits for all three backends, diff
  through `report_event`/`changed_device_fields` rather than hand-rolling a comparison, add the
  matching `UpdateField` variant plus a case in `events.rs`'s per-field test (a field with no variant
  is invisible in reports), gate module + entry point + examples behind a new feature, then check
  every combination (`--no-default-features --features <each>`) and
  `cargo clippy --workspace --all-targets` — the workspace form, not `-p`, is the comparable count.
- A background task must be stoppable: each backend has a `Drop` that clears the scan/advertising
  flag, aborts the notification pumps and calls `NotificationHub::forget_all`, because the task holds
  `Arc` clones and dropping the monitor is its only stop signal. Any new task needs the same.
- Never hold a `DashMap` guard across an `.await`, and never `get_mut` the same shard while
  iterating: read, clone, drop. A `watch::Sender::send` with no receivers drops the value, so state
  that a task reads at startup must go in with `send_replace`.

## Verify

```bash
TMPDIR=/workspace/target/tmp cargo test -p bt_mon --all-features
# 129 unit + 4 integration + 29 doctests (3 ignored) pass on rustc 1.98.1, zero warnings
TMPDIR=/workspace/target/tmp cargo check -p bt_mon --no-default-features --features mock
cargo run --example mock_backend_demo --features mock
```

Every feature combination has to compile on its own — `--no-default-features` alone (types and
traits), then `btleplug`, `bluer`, `mock`, `full` — because the backends differ as above and a cfg
mistake is only visible in the combination that hits it. `cargo doc -p bt_mon --no-deps` is clean;
rustdoc warnings in this crate are real, since the docs are what tells a contributor which backend
does what.

Runtime behaviour on a real radio is not verifiable here. `cargo run --example bluer_scan_and_gatt
--features bluer --no-default-features` is the harness for that (scan, connect, discover, read,
notify, health counters), and `TECH_DEBT.md` lists which claims are still waiting on hardware.
