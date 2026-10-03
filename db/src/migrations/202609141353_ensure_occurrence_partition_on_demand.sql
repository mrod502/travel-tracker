--migrate:up.begin
-- =====================================================================
-- Repair a missing occurrences partition at the moment it is noticed.
--
-- Why: 202609021200 created 16 months of partitions and made `db up`
-- extend that horizon on every deploy. Neither of those helps a node
-- that outlives the horizon without redeploying, or one handed a row
-- dated outside it — a synced backlog, an occurrence captured while its
-- clock was wrong, a peer whose clock is wrong. Such a node dies at the
-- partition boundary with
--
--   ERROR: no partition of relation "occurrences" found for row
--   DETAIL: Partition key of the failing row contains (observed_at) = (...)
--
-- i.e. a schema problem reported at the worst possible moment, by the
-- code least able to do anything about it.
--
-- This migration supplies the mechanism the repository's INSERT retries
-- against, and factors the DDL so exactly one place builds a partition:
--
--   occurrence_partition_name(ts)        — which monthly partition owns ts
--   ensure_occurrence_partition_at(ts)   — create one, given its month start
--   ensure_occurrence_partition(ts)      — create the one that holds ts
--   ensure_occurrence_partitions(n)      — reimplemented over the above; the
--                                          horizon entry point `db up` calls
--
-- Deliberate choices:
--
--   No DEFAULT partition, unchanged from the previous migration. A DEFAULT
--   turns "the schedule did not fire" into rows quietly filed somewhere off
--   the monthly scheme, and you cannot attach the right partition afterwards
--   without moving data out of DEFAULT first.
--
--   A bounded window on the on-demand path. `observed_at` is a claim made by
--   a row, and once other nodes can hand us rows it is no longer our own
--   clock: "create whatever partition the timestamp asks for" is a way to
--   fill pg_class from the network. A row claiming to be from 2387 — or from
--   a decade before this deployment — is bad data, not a missing schedule,
--   and it should fail loudly rather than get a home.
--
--   Everything is computed in UTC, not in the session time zone. The function
--   this replaces truncated `now()` in whatever zone the session happened to
--   use, so a session with TimeZone = Asia/Tokyo would have named and bounded
--   a month differently from a node writing in UTC, and whichever of the two
--   ran second would have hit an overlapping-bound error. Names and bounds now
--   agree regardless of who asks.
-- =====================================================================

-- ---------------------------------------------------------------------
-- The name of the monthly partition that owns an instant.
-- ---------------------------------------------------------------------
CREATE OR REPLACE FUNCTION occurrence_partition_name(ts timestamptz)
    RETURNS text
    LANGUAGE sql
    STABLE
    STRICT
    AS $fn$
    SELECT 'occurrences_'
        || to_char(date_trunc('month', ts AT TIME ZONE 'UTC'), 'YYYY_MM');
$fn$;

COMMENT ON FUNCTION occurrence_partition_name(timestamptz) IS
    'Name of the monthly occurrences partition that holds ts, e.g.
     2026-09-14T23:10:00Z -> occurrences_2026_09. UTC-based whatever the
     session time zone is, because partitions are named after the UTC month
     their bounds open on.';

-- ---------------------------------------------------------------------
-- The only place that builds a partition.
--
-- month_start is the first instant of a month on the UTC wall clock, e.g.
-- date_trunc('month', now() AT TIME ZONE 'UTC'). Returns true when this
-- call created the partition, false when it was already there.
-- ---------------------------------------------------------------------
CREATE OR REPLACE FUNCTION ensure_occurrence_partition_at(month_start timestamp)
    RETURNS boolean
    LANGUAGE plpgsql
    AS $fn$
DECLARE
    part_name text := 'occurrences_' || to_char(month_start, 'YYYY_MM');
BEGIN
    IF month_start IS NULL THEN
        RAISE EXCEPTION 'ensure_occurrence_partition_at requires a month start'
            USING ERRCODE = 'null_value_not_allowed';
    END IF;

    IF month_start <> date_trunc('month', month_start) THEN
        -- 22023, parameter_out_of_range, spelled as the raw SQLSTATE: PL/pgSQL
        -- has no condition name for it, and a name it does not recognise is a
        -- hard error at RAISE time rather than a fallback.
        RAISE EXCEPTION 'ensure_occurrence_partition_at wants the first instant '
            'of a month, got %', month_start
            USING ERRCODE = '22023';
    END IF;

    -- One creation at a time. Without this, the first insert of a new month
    -- becomes N concurrent CREATE TABLEs, each taking ACCESS EXCLUSIVE on the
    -- partitioned parent, and every writer queues behind all of them. With it
    -- the losers of the race take the advisory lock, find the partition
    -- already there, and leave. The lock drops at end of transaction, which is
    -- also when the new partition becomes visible, so a waiter cannot miss it.
    PERFORM pg_advisory_xact_lock(hashtext('occurrences_partition_ddl'));

    IF to_regclass('public.' || part_name) IS NULL THEN
        BEGIN
            -- Both the name and the bounds come out of date_trunc, never from
            -- a caller's string, so %I/%L have nothing to escape here. The
            -- bounds are instants: `timestamp AT TIME ZONE 'UTC'` reads a UTC
            -- wall-clock time as an absolute moment, so a session in another
            -- zone still creates exactly the same range.
            EXECUTE format(
                'CREATE TABLE IF NOT EXISTS %I PARTITION OF occurrences
                    FOR VALUES FROM (%L) TO (%L)',
                part_name,
                month_start AT TIME ZONE 'UTC',
                (month_start + interval '1 month') AT TIME ZONE 'UTC'
            );
            RETURN true;
        EXCEPTION
            -- 42P07 and 23505 are what two concurrent CREATE TABLE IF NOT
            -- EXISTS for the same name actually raise, the second one often
            -- through the pg_type index rather than the table name. The
            -- advisory lock above makes this path rare rather than common; it
            -- stays because a restore or a second node can arrive with no
            -- shared lock state to lose the race against.
            WHEN duplicate_table OR unique_violation THEN
                RETURN false;
        END;
    END IF;

    RETURN false;
END;
$fn$;

COMMENT ON FUNCTION ensure_occurrence_partition_at(timestamp) IS
    'Create the monthly occurrences partition beginning at month_start (UTC,
     first instant of a month) unless it already exists; true if this call
     created it. Serialised against other creators by an advisory lock keyed
     on hashtext(''occurrences_partition_ddl''). Creates no indexes of its own
     — the partitioned parent propagates them — and never drops anything:
     retention is a separate, deliberate operation.';

-- ---------------------------------------------------------------------
-- The on-demand heal: create the one partition that holds an instant.
-- This is what a failed INSERT calls before it retries.
-- ---------------------------------------------------------------------
CREATE OR REPLACE FUNCTION ensure_occurrence_partition(ts timestamptz)
    RETURNS text
    LANGUAGE plpgsql
    AS $fn$
DECLARE
    horizon     timestamp := date_trunc('month', now() AT TIME ZONE 'UTC');
    month_start timestamp;
BEGIN
    IF ts IS NULL THEN
        RAISE EXCEPTION 'ensure_occurrence_partition(null) cannot name a partition'
            USING ERRCODE = 'null_value_not_allowed';
    END IF;

    month_start := date_trunc('month', ts AT TIME ZONE 'UTC');

    -- The window: twelve months back for a backlog arriving late, twenty-four
    -- forward, which is the deploy horizon (15) with room for a node whose
    -- clock runs a little ahead. Outside it a row is not evidence that a
    -- partition is missing, it is evidence that the row is wrong.
    IF month_start < horizon - interval '12 months'
        OR month_start > horizon + interval '24 months' THEN
        RAISE EXCEPTION 'refusing to create an occurrences partition for %: '
            'outside % .. %', ts,
            horizon - interval '12 months',
            horizon + interval '24 months'
            USING ERRCODE = '22023';
    END IF;

    PERFORM ensure_occurrence_partition_at(month_start);

    RETURN occurrence_partition_name(ts);
END;
$fn$;

COMMENT ON FUNCTION ensure_occurrence_partition(timestamptz) IS
    'Create the monthly occurrences partition that holds ts and return its
     name. Idempotent. Refuses with SQLSTATE 22023 for a ts more than
     12 months before or 24 months after the current month, so a bad row
     cannot manufacture partitions. OccurrenceRepository::create calls this
     once, and retries the INSERT, when the INSERT fails with "no partition of
     relation occurrences found for row".';

-- ---------------------------------------------------------------------
-- The scheduled horizon, now built out of the helper above instead of
-- repeating the DDL. Same signature, same return (how many were created),
-- same behaviour: every month from the current one through +months_ahead.
-- ---------------------------------------------------------------------
CREATE OR REPLACE FUNCTION ensure_occurrence_partitions(months_ahead integer)
    RETURNS integer
    LANGUAGE plpgsql
    AS $fn$
DECLARE
    base    timestamp := date_trunc('month', now() AT TIME ZONE 'UTC');
    created integer := 0;
    i       integer;
BEGIN
    IF months_ahead < 0 THEN
        RAISE EXCEPTION 'months_ahead must not be negative (got %)', months_ahead
            USING ERRCODE = '22023';
    END IF;

    FOR i IN 0..months_ahead LOOP
        created := created
            + ensure_occurrence_partition_at(base + make_interval(months => i))::integer;
    END LOOP;

    RETURN created;
END;
$fn$;

COMMENT ON FUNCTION ensure_occurrence_partitions(integer) IS
    'Create any missing monthly occurrences partition in the range
     [UTC month of now(), + months_ahead], returning how many were actually
     created. Idempotent and safe to call on every deploy: db up calls it
     after applying migrations.

     Installations that run for longer than the horizon without a redeploy
     need this called on a schedule too, e.g. with pg_cron:
       SELECT cron.schedule($$occurrence-partitions$$,
                            $$0 3 25 * *$$,
                            $$SELECT ensure_occurrence_partitions(4)$$);
     Running it on the 25th gives a week of slack before the next month
     opens. Since 202609141353 a node that outruns the horizon anyway heals
     the partition it needs at insert time, so a missed schedule costs a
     latency spike on the first insert of the month instead of the write
     path.';
--migrate:up.end

--migrate:down.begin
-- Revert the statements above.
--
-- Order matters: `ensure_occurrence_partitions` is put back as the
-- self-contained body 202609021200 wrote first, because `db up` calls it after
-- every migration and it must not be left delegating to a helper that is about
-- to be dropped. Only then do the three functions this migration added go.
--
-- Partitions a heal created stay attached to `occurrences`. They may hold rows,
-- and deleting a partition is deleting the observations inside it; they belong
-- to `occurrences` and leave with
-- 202607312147_create_bluetooth_occurrences, not with this one.
--
-- Consequence of reverting: a node that outruns its partition horizon gets the
-- dead write path back, and the repository's insert retry has nothing to call
-- — it will report "function ensure_occurrence_partition(timestamp with time
-- zone) does not exist" on top of the original insert failure, which is the
-- truth and the first thing to check. Re-applying this migration restores the
-- behaviour; nothing has to be undone in between.

-- 1. Back to the body 202609021200 shipped, verbatim.
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
                EXECUTE format(
                    'CREATE TABLE IF NOT EXISTS %I PARTITION OF occurrences
                        FOR VALUES FROM (%L) TO (%L)',
                    part_name, month_from, month_to
                );
                created := created + 1;
            EXCEPTION
                WHEN duplicate_table OR unique_violation THEN
                    NULL;
            END;
        END IF;
    END LOOP;

    RETURN created;
END;
$fn$;

-- 2. The objects this migration owns.
DROP FUNCTION IF EXISTS ensure_occurrence_partition(timestamptz) RESTRICT;
DROP FUNCTION IF EXISTS ensure_occurrence_partition_at(timestamp) RESTRICT;
DROP FUNCTION IF EXISTS occurrence_partition_name(timestamptz) RESTRICT;
--migrate:down.end
