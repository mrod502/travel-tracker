-- Revert 202609141353_ensure_occurrence_partition_on_demand.sql
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
