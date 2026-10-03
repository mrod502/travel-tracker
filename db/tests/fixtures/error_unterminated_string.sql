-- The string literal never closes, so the statement it belongs to is never
-- finished: a distinct lex error, not a truncated statement.
INSERT INTO t (note) VALUES ('open and never closed;
