-- `up` is still open when the second `up.begin` arrives: a duplicate open, not
-- a second definition.
--migrate:up.begin
CREATE TABLE t (id INT);
--migrate:up.begin
CREATE TABLE u (id INT);
--migrate:up.end
