--the following line tells the parser that the following block is the 'up' segment of the migration
--migrate:up.begin
    CREATE TABLE users (
        id UUID PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(),
        name TEXT NULL
    );
-- this tells the parser to run the next statement without a transaction
--migrate:skipTx
CREATE INDEX CONCURRENTLY idx_users_name on users(name);
-- the following line tells the parser this is the end of the migrate's 'up' segment
--migrate:up.end
--the following line tells the parser that the following block is the 'down' segment of the migration
--migrate:down.begin
DROP INDEX idx_users_name;
DROP TABLE users;
-- the following line tells the parser this is the end of the migrate's 'down' segment
--migrate:down.end
