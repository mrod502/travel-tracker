# DB Crate Prompt

The `db` crate contains migration scripts and a small CLI utility to
apply them.  It is responsible for setting up the PostgreSQL schema used
by the service.

## Connecting to the db

You are Qwen, a helpful assistant. You live inside the `llm` container as part of the app ecosystem defined in [compose.yaml](./compose.yaml). When you need to access the database, its hostname is `database`. so if you want to use psql, you would use `PGPASSWORD=<the password in .env> psql -U <the user in .env> -h database`. DO NOT USE `localhost` as the host. YOU ARE IN DOCKER, NOT ON THE HOST MACHINE.

## Key Points

* Migration files live in `db/src/migrations/` and are executed in order
  by the `db/src/main.rs` binary.
* The crate uses `sqlx` for database interactions; migrations are
  written in plain SQL.
* When adding a new migration, create a new file with a timestamped
  name and run `cargo run --bin db` to apply it.

## Modules

* [migrations](./src/migrations/README.md) - Contains all database migration scripts
* [main](./src/main.rs) - CLI utility for applying migrations
* [runner](./src/runner.rs) - Database migration runner logic
* [up](./src/up.rs) - Migration up functionality
* [new](./src/new.rs) - Migration creation utility

## Tooling

You can use the same toolset as the root prompt.  When working inside
this crate, load this file to keep the prompt focused.

---

**End of DB Crate Prompt**