-- Run once as the cluster administrator with psql -X --set ON_ERROR_STOP=1.
-- Deliberately fail on existing names: do not adopt roles/databases with unknown grants.
-- Install staging.pg_hba.conf BEFORE broader allow rules before enabling LOGIN.
CREATE ROLE uob_staging NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE
    NOINHERIT NOREPLICATION NOBYPASSRLS CONNECTION LIMIT 4;
CREATE DATABASE uob_staging TEMPLATE template0;
REVOKE ALL ON DATABASE uob_staging FROM PUBLIC;
GRANT CONNECT ON DATABASE uob_staging TO uob_staging;
\connect uob_staging
REVOKE ALL ON SCHEMA public FROM PUBLIC;
CREATE SCHEMA staging;
GRANT USAGE ON SCHEMA staging TO uob_staging;
-- This is a test-only sink, not the production exporter's future schema.
CREATE TABLE staging.observations (
    environment text NOT NULL CHECK (environment = 'staging'),
    import_id uuid NOT NULL,
    record_sequence integer NOT NULL CHECK (record_sequence >= 0),
    station_slot integer NOT NULL CHECK (station_slot BETWEEN 0 AND 15),
    status text NOT NULL CHECK (status IN ('Available', 'Unavailable', 'Faulted')),
    PRIMARY KEY (import_id, record_sequence)
);
REVOKE ALL ON ALL TABLES IN SCHEMA staging FROM PUBLIC;
GRANT SELECT, INSERT ON staging.observations TO uob_staging;
ALTER ROLE uob_staging IN DATABASE uob_staging SET search_path = staging, pg_catalog;
ALTER ROLE uob_staging SET statement_timeout = '5s';
ALTER ROLE uob_staging SET idle_in_transaction_session_timeout = '5s';
-- No password or LOGIN is set by this file. Activate only after isolation validation.
