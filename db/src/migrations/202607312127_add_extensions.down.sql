-- Revert 202607312127_add_extensions.sql
--
-- RESTRICT, not CASCADE: if anything still depends on these extensions, the
-- migrations that created that something have not been reverted yet, and
-- silently cascading would delete tables nobody asked to delete. The revert
-- fails and says so instead.
DROP EXTENSION IF EXISTS h3 RESTRICT;
DROP EXTENSION IF EXISTS postgis_raster RESTRICT;
DROP EXTENSION IF EXISTS postgis RESTRICT;
DROP EXTENSION IF EXISTS pgcrypto RESTRICT;
