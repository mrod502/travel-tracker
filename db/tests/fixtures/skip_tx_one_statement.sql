--migrate:up.begin
CREATE TABLE events (id BIGINT PRIMARY KEY, name TEXT NOT NULL);
--migrate:skipTx
CREATE INDEX CONCURRENTLY idx_events_name ON events (name);
-- No second skipTx: this index is created inside the transaction, and the
-- option must not have leaked from the statement above.
CREATE INDEX idx_events_name_plain ON events (name);
--migrate:up.end
--migrate:down.begin
DROP TABLE events;
--migrate:down.end
