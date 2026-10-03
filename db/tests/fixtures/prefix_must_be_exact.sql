--migrate:up.begin
SELECT migrate:left FROM nowhere;
-- migrate:up.end
--MIGRATE:up.end
--	migrate:up.end
-- None of the three lines above closes the `up` block: the prefix has to be
-- exactly `--migrate:`, so they are ordinary comments and the statement below
-- is still `up` content. `migrate:left` on the first line has no `--` at all,
-- so it is not directive-recognized either.
SELECT 2;
--migrate:up.end
