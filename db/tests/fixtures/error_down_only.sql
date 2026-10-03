-- A migration that only says how to undo something. `up` is required, so this
-- is refused rather than recorded as applied while changing nothing.
--migrate:down.begin
DROP TABLE t;
--migrate:down.end
