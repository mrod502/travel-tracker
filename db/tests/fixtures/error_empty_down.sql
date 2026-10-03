-- A `down` block that declares nothing: a revert that would silently undo
-- nothing. Write no `down` block at all if the migration cannot be reverted.
--migrate:up.begin
CREATE TABLE t (id INT);
--migrate:up.end
--migrate:down.begin
--migrate:down.end
