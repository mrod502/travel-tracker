# Migrations

SQL files that define the travel-tracking schema, applied in timestamp order by
the `db` binary.

## The contract

A migration is a pair of files sharing one timestamp:

```
<YYYYMMDDHHMM>_<description>.sql        the change
<YYYYMMDDHHMM>_<description>.down.sql   the undo
```

* Timestamp is 12 digits, `YYYYMMDDHHMM`; the description is `snake_case`.
* Files are ordered by timestamp, so **a new migration gets a new timestamp —
  never edit an already-applied file** to change the schema.
* `db new-migration <description>` writes both halves.

### Revert scripts

`db down` reverts one migration at a time, newest first. For each entry in the
`migrations` table it runs `<same-stem>.down.sql` and deletes that row **in one
transaction**, so a revert either fully happens or not at all.

Writing a good revert:

* **Prefer `RESTRICT` over `CASCADE`.** If something unexpected depends on the
  object, `RESTRICT` fails the revert loudly; `CASCADE` drops the dependent
  quietly and the next `db up` has to guess what was lost.
* Anything later migrations built on top of this one is already reverted when
  your script runs, so you only have to undo your own objects.
* Reverting a table deletes its rows. There is no undo for the data, only the
  schema — `db down --dry-run` marks the steps that lose rows.
* An empty revert script is refused rather than run: forgetting the registry row
  without changing the schema would leave the database claiming to be something
  it isn't.

The `migrations` registry itself is the floor. `202607312100_create_migrations_registry.sql`
creates the table that records what has been applied, so `db down` never reverts
it — `down --number 0` stops with that one row left in place.

## Migrations in this directory

| Migration | What it does |
|-----------|--------------|
| `202607312100_create_migrations_registry` | Creates the `migrations` table. The floor of `db down`. |
| `202607312127_add_extensions` | `pgcrypto`, `postgis`, `postgis_raster`, `h3` |
| `202607312137_create_bluetooth_occurrence_types` | `signal_type`, `node_type`, `node_status`, `ble_address_type`, `adv_type`, `location_source`, `sync_direction` |
| `202607312146_create_nodes` | The `nodes` table and its indexes |
| `202607312147_create_bluetooth_occurrences` | The `occurrences` table (append-only observations) and `occurrence_relays` |
| `202608020206_create_occurrence_indexes` | Query-path indexes, including the H3 generated-column indexes |
| `202608020247_create_sync_cursors` | The `sync_cursors` table for federation state |
| `202608260000_create_node_revocations` | The `node_revocations` ledger (reverting it deletes the revocation audit trail) |
| `202609021200_partition_occurrences_forward` | Adds `ensure_occurrence_partitions(months_ahead)` and creates the monthly partitions that were running out |
| `202609041200_fix_geo_cell_coordinate_order` | Fixes the `geo_cell_*` generated columns to read longitude first |
| `202609141353_ensure_occurrence_partition_on_demand` | Adds `ensure_occurrence_partition(ts)`, so a write dated outside the provisioned horizon creates the month it needs instead of failing; rebuilds `ensure_occurrence_partitions` on top of it |
| `202609142142_create_revocation_status_lists` | The `revocation_status_lists` publication table — one signed RSL per `(issuer_id, sequence_number)`, which is what makes the per-CA anti-replay counter durable. Reverting it deletes the published lists and restarts every counter at 1 |

## Applying them

```bash
cargo run --bin db -- --host <host> --user <user> --db <db> up          # all pending
cargo run --bin db -- --host <host> --user <user> --db <db> up --number 1
cargo run --bin db -- --host <host> --user <user> --db <db> up --dry-run
```

Each migration runs in its own transaction together with its registry row, and
is applied at most once. See [../../AGENTS.md](../../AGENTS.md) for `down`,
`reset`, and the guardrails around destructive runs.

## Note for tooling

`.down.sql` files are revert scripts, not migrations — `db up` skips them.
Statement splitting uses `sqlparser`, falling back to a semicolon splitter that
understands dollar-quoted bodies, so `$$ ... $$` function definitions in a
migration are fine.
