-- =====================================================================
-- The runner's own registry, as the first migration.
--
-- This table used to be created by application code (a CREATE TABLE IF
-- NOT EXISTS in main(), duplicated in the reset path). That put the one
-- relation that *defines* schema history outside the history it defines:
-- it could not be inspected as part of a migration, and reset had to drop
-- and re-create it by hand because nothing owned it.
--
-- Timestamped ahead of every other migration so a database built from this
-- directory creates the registry before anything can be recorded in it. The
-- runner bootstraps by applying exactly this file when the table is absent
-- (see src/registry.rs); the DDL lives here and nowhere else.
-- =====================================================================

CREATE TABLE IF NOT EXISTS migrations (
    id           BIGSERIAL PRIMARY KEY NOT NULL,
    -- Basename without the .sql suffix, i.e. everything after the timestamp.
    name         TEXT NOT NULL UNIQUE,
    -- Timestamp parsed from the migration's file name.
    created_at   TIMESTAMPTZ NOT NULL,
    -- When this runner applied it.
    executed_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE migrations IS
    'Applied-migration log for the db CLI: one row per migration file that has been applied. db up inserts the row inside the same transaction as the migration itself, so a rolled-back migration leaves no row, and db down removes the row when it reverts. This table is itself migration 202607312100 and is the floor of db down — reverting it would delete the record of what is applied, so it has no .down.sql.';
