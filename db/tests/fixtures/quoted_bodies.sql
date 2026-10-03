-- Every quoting form a migration can use, each hiding a `;` from the splitter.
SELECT 'a; b' AS quoted;

SELECT "odd;identifier" FROM (SELECT 1 AS "odd;identifier") AS t;

CREATE FUNCTION untaged_body() RETURNS text LANGUAGE sql AS $$
    SELECT 'inner; semicolon';
$$;

-- A tagged body: `--migrate:skipTx` below is body text, not a directive, and
-- the `;` inside stays inside.
CREATE FUNCTION tagged_body() RETURNS integer LANGUAGE plpgsql AS $fn$
DECLARE
    total integer := 0;
BEGIN
    --migrate:skipTx
    total := total + 1;
    RETURN total;
END;
$fn$;

SELECT 'it''s; still one statement' AS escaped_quote;
