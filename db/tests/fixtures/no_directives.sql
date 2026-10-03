-- A plain migration file: nothing in it is a directive, so every statement
-- belongs to `up` and there is no `down` at all. This is the shape of every
-- migration that predates the directive format.
CREATE TABLE gauges (
    id UUID PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(),
    reading NUMERIC NOT NULL,
    -- a comment with a semicolon; in it
    observed_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_gauges_observed_at ON gauges (observed_at);

/* A block comment that also has a semicolon; and
   spans lines. */
COMMENT ON TABLE gauges IS 'readings that were never a directive';
