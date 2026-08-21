# DB Crate Prompt

The `db` crate contains migration scripts and a small CLI utility to
apply them.  It is responsible for setting up the PostgreSQL schema used
by the service.

## Connecting to the Database

You are running inside the `llm` Docker container as part of the app ecosystem defined in [compose.yaml](./../compose.yaml). When you need to access the database, its hostname is `database`.

**To connect via psql from inside the container:**
```bash
source .env  # or export the variables
psql -U ${DB_USER} -h database -d ${DB_NAME}
```

**DO NOT USE `localhost` as the host** — use `database` as the hostname when connecting from within the container.

## Schema Overview

### Enum Types

The schema uses PostgreSQL enum types for compact, type-safe storage:

| Enum Type | Values | Used In |
|-----------|--------|---------|
| `node_type` | `full`, `light`, `aggregator`, `signal` | `node.node_type` |
| `node_status` | `active`, `suspected`, `down`, `revoked` | `node.status` |
| `ble_address_type` | `public`, `random_static`, `random_resolvable`, `random_nonresolvable` | `occurrence.address_type` |
| `adv_type` | `connectable_adv`, `scannable_adv`, `broadcast_adv`, `extended_adv` | `occurrence.adv_type` |
| `location_source` | `node_fixed`, `node_gps`, `interpolated`, `aggregator_fixed` | `occurrence.location_source` |
| `sync_direction` | `inbound`, `outbound` | `sync_cursor.direction` |

### Core Tables

| Table | Purpose |
|-------|---------|
| `node` | Registry of network peers with signing keys and CA credentials |
| `occurrence` | Append-only Bluetooth device observations (time-partitioned) |
| `sync_cursor` | Per-peer replication progress tracking |

### Extensions Required

- `pgcrypto` - for `gen_random_uuid()`
- `postgis` - for geography/geometry types
- `postgis_raster` - required dependency of postgis
- `h3` - for H3 geospatial indexing
- `h3_postgis` - bridges h3 and postgis types

## Migration Management

* Migration files live in `db/src/migrations/` and are executed in order
  by the `db/src/main.rs` binary.
* The crate uses `sqlx` for database interactions; migrations are
  written in plain SQL.
* When adding a new migration, create a new file with a timestamped
  name and run `cargo run --bin db` to apply it.

### Migration File Naming Convention

Migrations must follow this naming pattern:
```
<YYYYMMDDHHMM>_<description>.sql
```

Example: `202607312147_create_occurrence_table.sql`

- **YYYYMMDDHHMM**: Timestamp in UTC (year, month, day, hour, minute)
- **description**: snake_case description of the migration
- **Extension**: Must be `.sql`

## CLI Commands

### Apply Pending Migrations
```bash
# From workspace root
cargo run --bin db -- --host database --port 5432 --user <user> --db <dbname> up --migrations-path ./db/src/migrations

# From inside the llm container (after sourcing .env)
cargo run --bin db -- --host database --port 5432 --user ${DB_USER} --db ${DB_NAME} up --migrations-path ./db/src/migrations
```

### Reset Database (Drop and Re-apply All Migrations)
**Warning**: This will drop all user-defined tables, custom types (enums, composite types, ranges), and the `migrations` table, then re-apply all migrations from scratch. All data will be lost!

The reset process:
1. Drops all custom types (ENUMs, composite types, RANGEs) in the public schema (excluding extension-owned types)
2. Drops all tables in the public schema (excluding extension-owned tables like `spatial_ref_sys`)
3. Recreates the `migrations` table
4. Re-applies all migrations from scratch

**Note**: PostgreSQL extensions (pgcrypto, postgis, h3, etc.) are preserved because migrations use `CREATE EXTENSION IF NOT EXISTS` which is idempotent.

```bash
# From workspace root
cargo run --bin db -- --host database --port 5432 --user <user> --db <dbname> reset --migrations-path ./db/src/migrations

# From inside the llm container (after sourcing .env)
cargo run --bin db -- --host database --port 5432 --user ${DB_USER} --db ${DB_NAME} reset --migrations-path ./db/src/migrations
```

### Create New Migration
```bash
cargo run --bin db -- new-migration --name <description>
```

## Modules

* [migrations](./src/migrations/README.md) - Contains all database migration scripts
* [main](./src/main.rs) - CLI utility for applying migrations
* [runner](./src/runner.rs) - Database migration runner logic
* [up](./src/up.rs) - Migration up functionality (discovers and applies pending migrations)
* [down](./src/down.rs) - Migration reset functionality (drops custom types, tables, and re-applies migrations)
* [new](./src/new.rs) - Migration creation utility
* [file_attrs](./src/file_attrs.rs) - File attribute parsing for migration ordering

## Testing

### Running Tests
```bash
# All tests
cargo test -p db

# Unit tests only
cargo test -p db --lib

# Integration tests
cargo test -p db --test integration_test
```

### Test Coverage

The migration system has comprehensive tests covering:

1. **Filename Parsing** (`src/up.rs` tests)
   - Valid timestamp parsing
   - Invalid extension handling
   - Invalid timestamp format rejection

2. **Query Splitting** (`src/up.rs` tests)
   - Multi-statement SQL splitting
   - Empty statement filtering
   - Whitespace preservation

3. **File Ordering** (integration tests)
   - Timestamp-based ordering
   - Path-based tiebreaking
   - Name-based final tiebreaking

4. **Error Handling** (integration tests)
   - File not found errors
   - Invalid migration format errors
   - Transaction rollback on failure

### Test Philosophy

- **Unit tests** use no external dependencies (in-memory/file-based)
- **Integration tests** mock database behavior without requiring a running PostgreSQL instance
- **No hardcoded secrets** in test files - use environment variables or mocks
- All tests must pass in CI/CD environments without database access

## Important Notes

* **Migrations are atomic**: Each migration runs in a transaction. If any statement fails, the entire migration rolls back.
* **Migrations are append-only**: The `occurrence` table and related data are never updated after insertion.
* **Partition management**: Monthly partitions are created manually or via automation (pg_partman recommended for production).
* **H3 function signatures**: The `h3-pg` extension function names have varied across versions. Verify against your installed version before writing migrations that use H3 functions.
* **SQL parsing**: The migration system uses `sqlparser` (v0.51) to parse SQL statements. For PostgreSQL-specific syntax that sqlparser may not fully support (e.g., `uuidv7()`, custom extensions, complex generated columns), the system falls back to simple semicolon-based splitting. This ensures migrations with advanced PostgreSQL features still work correctly.

## Troubleshooting

### Migration fails with "extension already exists"
This is expected - the migration uses `CREATE EXTENSION IF NOT EXISTS` which is idempotent. The NOTICE message can be ignored.

### Migration fails with "relation already exists"
Check if the migration was partially applied. If so, you may need to manually clean up before re-running.

### H3 function not found
Verify the `h3-pg` version and check its documentation for the correct function signatures. The API has changed between versions.

### SQL parser warnings or fallback activation
The migration system may log messages about sqlparser failing or using fallback splitting. This is expected for PostgreSQL-specific syntax like `uuidv7()`, custom extension functions, or complex generated columns. The fallback mechanism ensures these migrations still execute correctly.

### Long migration files fail to parse
If a migration file is very large or contains complex nested SQL, sqlparser may hit internal limits. The system automatically falls back to semicolon-based splitting in such cases. If issues persist, consider splitting the migration into smaller, logically separated files.
