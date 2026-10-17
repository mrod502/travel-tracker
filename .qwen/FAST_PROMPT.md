# Fast Agent Context — BLE Occurrence Tracking

Load this first, then answer the question that was asked. Everything below is meant to be stable.
**Current progress is deliberately not in this file** — see "Where truth lives".

## What the system is

Nodes scan Bluetooth LE advertisements, stamp each sighting with an H3 location, sign it, and
append it to a Postgres occurrence row. The query shape is: *what did this node see, when, where*.

Invariants that do not move:

- **Self-certifying node identity** — `node_id` = SHA-256 of the node's Ed25519 signing public key.
- **An occurrence is a signed assertion** — canonical CBOR payload + signature, verifiable offline.
  Downstream derivation therefore never rewrites `occurrences`; derived tables are written separately.
- **Schema changes go through migrations only** (`db` crate). Never hand-edit a table.

## Workspace — six crates

| Crate | Role |
|-------|------|
| `app` | Node binary: CLI, capture → sign → store, CA operations, queries |
| `bt_mon` | BLE scanning library (backends: `btleplug`, `bluer`, `mock`) |
| `bt_iden` | Probabilistic device-identity resolution (evidence scoring, merge decisions) |
| `repo` | sqlx data layer — models and repositories |
| `db` | Migration tool (`up` / `down` / `reset` / `new-migration`) |
| `ca` | Certificate-authority library (enrollment, revocation list) |

## Where truth lives (newest first)

1. `GAP_ANALYSIS.md` — the live defect register (blocking / major / minor, with evidence).
2. `.knowledge/implementation/status.md` — component-by-component summary.
3. `.knowledge/implementation/roadmap.md` — phase plan.
4. Dated files under `gap_analysis/` and `.knowledge/implementation/` are snapshots; check the
   `Last updated:` line before quoting one.

`README.md` lists "Web Framework: Axum" — that is **aspirational**. There is no HTTP API and no web
framework in any `Cargo.toml`; the only operator interface is the `app` CLI. Do not go looking for
routes.

## Environment limits (this container)

- **No Bluetooth adapter.** Exercise the capture path with `--use-mock-backend` **and** `--features
  mock`. The flag itself is accepted by every build — `app --help` lists it without the feature — but
  the branch that constructs the simulated radio is what the feature compiles in, so a run that looks
  like it is on the mock backend is on btleplug. Never conclude a scan path works or fails from real
  radio behaviour here.
- **Postgres is reachable** at host `database:5432` (credentials in `.env.database`). Postgres +
  PostGIS + h3-pg are installed, so schema work can be verified for real.
- **`/tmp` is mounted `noexec`** — doctests fail with `Permission denied (os error 13)` unless
  `TMPDIR=/workspace/target/tmp`. (`cargo test --all-targets` skips doctests, so it looks fine.)
- **Cap link parallelism** with `-j 2` on workspace-wide `cargo test` / `clippy --all-targets`; at
  default parallelism the linker gets `signal 9 [Killed]`. That is memory, not a code fault.
- **No CI.** Never create `.github/` or workflow files.

## Map

- `app/src/main.rs`, `app/src/cli.rs` — entry point and the full clap surface (flags, subcommands)
- `app/src/app.rs` — FullNode orchestration
- `app/src/config/` — layered config (flag > file > env > default)
- `app/src/node/` — `full.rs`, `identity.rs`, `observation.rs`, `rate_limiter.rs`, `device_id.rs`, `revocation.rs`
- `app/src/identity/` — device identity: `fingerprint.rs`, `adapt.rs`, `store.rs`, `replay.rs`
- `app/src/position/` — position sources: `gps`, `fixed`, `mock`, `fallback`, `cache`
- `app/src/provenance/` — `encode.rs` (canonical CBOR), `payload.rs`, `sign.rs`, `verify.rs`
- `repo/src/models/`, `repo/src/repositories/`, `repo/src/geo.rs` (H3 cell types)
- `db/src/migrations/` — **the schema is authoritative for table shape**
- `ca/src/`, `bt_iden/src/`, `bt_mon/src/backends/`
- `.knowledge/AGENTS.md` — knowledge-base index; `AGENTS.md` (repo root) — Rust conventions and project rules

## Commands

```bash
cargo run --bin app -- --help
cargo run --bin app --features mock -- --use-mock-backend monitor   # scan path without an adapter
cargo run --bin app -- query --last "1h"
cargo run --bin app -- stats
cargo run --bin app -- identity-replay --last 24h   # read-only unless --write

cargo run --bin db -- new-migration <name>          # never edit applied migrations
cargo run --bin db -- --host database up            # full arg list: README.md:61

cargo fmt
TMPDIR=/workspace/target/tmp cargo clippy --workspace --all-targets -j 2
TMPDIR=/workspace/target/tmp cargo test --workspace -j 2
```

## Reporting contract

- Give `file:line` for every claim about code. Read it; do not infer it from a name.
- Separate **observed** from **inferred**. "Not found" is a valid and useful answer — say it
  instead of guessing.
- If a doc and the code disagree, report both and trust the code.
- Do not modify files, run migrations, or write to the database unless the task explicitly asks.
- Budget is tight: prefer targeted searches over whole-file reads, and do not paste large files
  into your answer.
