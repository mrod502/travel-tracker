-- The tagged body never gets its matching `$fn$`; `$$` and `$other$` are not it.
CREATE FUNCTION f() RETURNS integer LANGUAGE plpgsql AS $fn$
BEGIN
    RETURN 1;
END;
$$;
