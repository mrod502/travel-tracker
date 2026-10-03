--migrate:up.begin
CREATE TABLE t (id INT);
--migrate:skipTx
--migrate:up.end
--migrate:down.begin
DROP TABLE t;
--migrate:down.end
