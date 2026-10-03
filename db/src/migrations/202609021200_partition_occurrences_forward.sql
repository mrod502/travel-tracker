--migrate:up.begin
-- =====================================================================
-- Occurrence partitions through the end of next year, and the mechanism
-- that stops them running out a second time.
--
-- Why this migration exists: `occurrences` is PARTITION BY RANGE
-- (observed_at) and the original schema created exactly two partitions,
-- occurrences_2026_07 and occurrences_2026_08. Measured against a live
-- server on 2026-09-02, every insert dated September 2026 onward was
-- rejected with
--
--   ERROR: no partition of relation "occurrences" found for row
--   DETAIL: Partition key of the failing row contains (observed_at) = (2026-09-02 ...)
--
-- i.e. the whole write path was dead. The comment in
-- 202607312147_create_bluetooth_occurrences.sql said "automate creation
-- via cron/pg_partman in practice" and nothing ever did.
--
-- Two parts, deliberately:
--   1. ensure_occurrence_partitions(months_ahead) — idempotent, creates
--      every monthly partition from the current month forward.
--   2. a call to it here, creating partitions through 2027-12.
-- The runner calls the same function after every `db up`, so a deployment
-- extends its own horizon. A long-lived install that never re-runs `db up`
-- should still schedule it (pg_cron, a systemd timer, whatever the
-- deployment has) — see COMMENT ON FUNCTION below.
--
-- No DEFAULT partition, on purpose. A DEFAULT partition turns "the
-- schedule did not fire" into rows silently landing somewhere off the
-- monthly scheme, and once a row is in DEFAULT you cannot attach the
-- partition that should have held it without moving data out first. A
-- failed insert that pages somebody is the safer failure.
-- =====================================================================

CREATE OR REPLACE FUNCTION ensure_occurrence_partitions(months_ahead integer)
    RETURNS integer
    LANGUAGE plpgsql
    AS $fn$
DECLARE
    base       date := date_trunc('month', now())::date;
    month_from date;
    month_to   date;
    part_name  text;
    created    integer := 0;
    i          integer;
BEGIN
    IF months_ahead < 0 THEN
        RAISE EXCEPTION 'months_ahead must not be negative (got %)', months_ahead;
    END IF;

    FOR i IN 0..months_ahead LOOP
        month_from := (base + make_interval(months => i))::date;
        month_to   := (base + make_interval(months => i + 1))::date;
        part_name  := 'occurrences_' || to_char(month_from, 'YYYY_MM');

        IF to_regclass('public.' || part_name) IS NULL THEN
            BEGIN
                -- Names are built from date_trunc output, never from user
                -- input, so %I/%L here have nothing to escape.
                EXECUTE format(
                    'CREATE TABLE IF NOT EXISTS %I PARTITION OF occurrences
                        FOR VALUES FROM (%L) TO (%L)',
                    part_name, month_from, month_to
                );
                created := created + 1;
            EXCEPTION
                -- 42P07 and 23505 are what two concurrent CREATE TABLE IF NOT
                -- EXISTS for the same name actually raise (the second one often
                -- via the pg_type index rather than the table name).
                WHEN duplicate_table OR unique_violation THEN
                    -- A concurrent caller won the race. The partition exists,
                    -- which is the outcome both callers wanted.
                    NULL;
            END;
        END IF;
    END LOOP;

    RETURN created;
END;
$fn$;

COMMENT ON FUNCTION ensure_occurrence_partitions(integer) IS
    'Create any missing monthly occurrences partition in the range
     [date_trunc(month, now()), date_trunc(month, now()) + months_ahead],
     returning how many were actually created. Idempotent and safe to call
     on every deploy: db up calls it after applying migrations.

     Installations that run for longer than the horizon without a redeploy
     need this called on a schedule too, e.g. with pg_cron:
       SELECT cron.schedule($$occurrence-partitions$$,
                            $$0 3 25 * *$$,
                            $$SELECT ensure_occurrence_partitions(4)$$);
     Running it on the 25th gives a week of slack before the next month
     opens. It creates no indexes beyond what the partitioned parent
     propagates automatically, and it never drops anything: retention is a
     separate, deliberate operation.';

-- Through the end of next year: i = 0 is the current month, i = 15 is
-- December of the following year, so 16 months of head start.
SELECT ensure_occurrence_partitions(15);
--migrate:up.end

--migrate:down.begin
-- Revert the statements above.
--
-- Drops the maintenance function only. The monthly partitions it created stay
-- attached to occurrences: they may hold rows, and deleting a partition is
-- deleting the observations inside it. The partitions belong to occurrences
-- and go with it when 202607312147_create_bluetooth_occurrences is reverted.
--
-- Consequence of reverting this alone: nothing creates next month's partition,
-- which is the state the migration exists to prevent. Re-applying it restores
-- the horizon immediately, because the function is idempotent.
DROP FUNCTION IF EXISTS ensure_occurrence_partitions(integer) RESTRICT;
--migrate:down.end
