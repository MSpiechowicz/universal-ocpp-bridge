-- Create fresh administration-owned names before applying version 1.
SELECT NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'runtime_role') AS role_is_new,
       NOT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = :'schema_name') AS schema_is_new
\gset
\if :role_is_new
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role already exists; refusing to adopt it'; END $failure$;
\endif
\if :schema_is_new
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'schema already exists; use upgrade only for a managed schema'; END $failure$;
\endif

CREATE ROLE :"runtime_role" NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
CREATE SCHEMA :"schema_name";
\ir migrations/001-canonical-events.sql

