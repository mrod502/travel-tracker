# Migrations

SQL files that define the travel-tracking schema, applied in timestamp order by
the `db` binary.

## The contract

One file per migration, holding both directions:

```
<YYYYMMDDHHMM>_<description>.sql    --migrate:up.begin … up.end   the change
                                    --migrate:down.begin … end    the undo
```

* Timestamp is 12 digits, `YYYYMMDDHHMM`; the description is `snake_case`.
* Files are ordered by timestamp, so **a new migration gets a new timestamp —
  never edit an already-applied file** to change the schema.
* `db new-migration <description>` writes the skeleton: an open, empty `up`
  block and a commented-out `down` block. Neither half is runnable until you
  write it, and `db up` refuses to record it as applied in that state.

### Revert blocks

`db down` reverts one migration at a time, newest first. For each entry in the
`migrations` table it runs that file's `--migrate:down` block and deletes the
row **in one transaction**, so a revert either fully happens or not at all.

The revert lives beside the change it undoes on purpose: a revert in a separate
file is a second source of truth, and the one thing nobody checks when the
forward half is edited.

Writing a good revert:

* **Prefer `RESTRICT` over `CASCADE`.** If something unexpected depends on the
  object, `RESTRICT` fails the revert loudly; `CASCADE` drops the dependent
  quietly and the next `db up` has to guess what was lost.
* Anything later migrations built on top of this one is already reverted when
  your block runs, so you only have to undo your own objects.
* Reverting a table deletes its rows. There is no undo for the data, only the
  schema — `db down --dry-run` marks the steps that lose rows.
* A `down` block with no statements in it is a parse error, and a file with no
  `down` block cannot be reverted: `db down` names every such migration and
  reverts nothing, rather than working through the convenient ones.

The `migrations` registry itself is the floor. `202607312100_create_migrations_registry.sql`
creates the table that records what has been applied, so `db down` never reverts
it — `down --number 0` stops with that one row left in place.

## Directives

Directives are comments of the exact form `--migrate:<key>`, optionally with a
`key=value` argument list:

```sql
--migrate:up.begin
    CREATE TABLE users (id UUID PRIMARY KEY NOT NULL DEFAULT gen_random_uuid());
    --migrate:skipTx
    CREATE INDEX CONCURRENTLY idx_users_name ON users (name);
--migrate:up.end

--migrate:down.begin
    DROP INDEX idx_users_name;
    DROP TABLE users;
--migrate:down.end
```

* `up.begin` / `up.end` / `down.begin` / `down.end` delimit the two halves of a
  migration. A file with no directives at all is still legal: every statement is
  `up` and there is no revert.
* `skipTx` marks the one statement that follows it to run outside a transaction
  (`CREATE INDEX CONCURRENTLY` and friends). It must be followed by a statement;
  dangling before a block end or at end of file is an error.
* The prefix is byte-exact. `-- migrate:up.begin` (a space after `--`) is an
  ordinary comment — which is how you write an example of a directive in prose,
  or comment one out; `--migrate: up.begin` (a space after the colon) is an
  error; a key nobody registered (`up.bgin`) is an error rather than a comment.

What the runner does with them:

* `db up` applies the `up` block; `db down` applies the `down` block. Both write
  the registry row in the same transaction as their statements, so a migration
  that fails halfway leaves neither.
* A group containing a `skipTx` statement cannot be one transaction, so it runs
  in three commits: the transactional statements, then each `skipTx` statement
  autocommitted in file order, then the registry row. A failure in the second or
  third commit leaves the first committed and the row unwritten — the migration
  is *not* marked applied and the next run will replay it against a schema it has
  partly changed. That is the trade `skipTx` buys; the error says which part
  failed. Use it for the statements that leave no choice.
* A `*.down.sql` file anywhere under the migrations directory stops `db up` and
  `db down` with a message naming it. Those files used to hold the revert; now
  that their contents belong in the migration, a leftover one is either dead
  weight or a migration that cannot be reverted.

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
| `202609271430_create_device_identity_tables` | The four derived identity tables — `device_identities`, `device_address_links`, `co_occurrence_events`, `association_edges` — and the `identity_resolution_method` enum. Nothing in the capture path writes here; the batch rebuilds all four from `occurrences`, so reverting loses current judgement, not data |

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

One file per migration: `db up`, `db down` and `db reset` all read
`*.sql`, and a `*.down.sql` anywhere among them is refused rather than ignored.

Statements are split by `db::Lexer` (`db/src/lex`), which knows no SQL dialect:
it tracks string literals, quoted identifiers, dollar-quoted bodies, nested
block comments and `--migrate:` lines in one pass, and cuts statements only at
top-level `;`. `sqlparser` is gone, so nothing a PostgreSQL extension allows can
defeat the splitter or make it re-render a statement instead of running the file
as written. A positional `$1` does not open a dollar quote.
