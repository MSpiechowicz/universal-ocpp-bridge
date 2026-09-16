-- Invoke through scripts/provision-postgresql.sh. Test fixtures may set target_version to 1.
SELECT
  (:'target_version'::integer BETWEEN 1 AND 2) AS valid_target,
  (:'target_version'::integer = 2) AS target_v2,
  (:'provision_mode' = 'install') AS install_mode,
  (:'provision_mode' = 'upgrade') AS upgrade_mode,
  (octet_length(:'schema_name') BETWEEN 1 AND 63) AS schema_name_valid,
  (octet_length(:'runtime_role') BETWEEN 1 AND 63) AS runtime_role_valid
\gset

\if :valid_target
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'target_version must be 1 or 2'; END $failure$;
\endif
\if :install_mode
\elif :upgrade_mode
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'provision_mode must be install or upgrade'; END $failure$;
\endif
\if :schema_name_valid
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'schema name must be between 1 and 63 bytes'; END $failure$;
\endif
\if :runtime_role_valid
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role must be between 1 and 63 bytes'; END $failure$;
\endif

BEGIN;

-- The destination must not offer ambient DDL, temporary objects, or public object access.
SELECT
  NOT EXISTS (
    SELECT 1
    FROM pg_database AS database
    CROSS JOIN LATERAL aclexplode(COALESCE(database.datacl, acldefault('d', database.datdba))) AS grant_item
    WHERE database.datname = current_database()
      AND grant_item.grantee = 0
      AND grant_item.privilege_type IN ('CREATE', 'TEMPORARY')
  ) AS database_public_safe,
  NOT EXISTS (
    SELECT 1
    FROM pg_namespace AS namespace
    CROSS JOIN LATERAL aclexplode(COALESCE(namespace.nspacl, acldefault('n', namespace.nspowner))) AS grant_item
    WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND namespace.nspname !~ '^pg_(toast|temp)'
      AND grant_item.grantee = 0
      AND grant_item.privilege_type = 'CREATE'
  ) AS schemas_have_no_public_create,
  NOT EXISTS (
    SELECT 1
    FROM pg_class AS relation
    JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
    CROSS JOIN LATERAL aclexplode(COALESCE(relation.relacl, acldefault('r', relation.relowner))) AS grant_item
    WHERE namespace.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND namespace.nspname !~ '^pg_(toast|temp)'
      AND grant_item.grantee = 0
  ) AS relations_have_no_public_grants,
  NOT EXISTS (
    SELECT 1
    FROM pg_attribute AS attribute
    JOIN pg_class AS relation ON relation.oid = attribute.attrelid
    JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
    CROSS JOIN LATERAL aclexplode(attribute.attacl) AS grant_item
    WHERE attribute.attnum > 0
      AND NOT attribute.attisdropped
      AND namespace.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND namespace.nspname !~ '^pg_(toast|temp)'
      AND grant_item.grantee = 0
  ) AS columns_have_no_public_grants,
  NOT EXISTS (
    SELECT 1
    FROM pg_proc AS routine
    JOIN pg_namespace AS namespace ON namespace.oid = routine.pronamespace
    CROSS JOIN LATERAL aclexplode(COALESCE(routine.proacl, acldefault('f', routine.proowner))) AS grant_item
    WHERE routine.prosecdef
      AND namespace.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND namespace.nspname !~ '^pg_(toast|temp)'
      AND grant_item.grantee = 0
      AND grant_item.privilege_type = 'EXECUTE'
  ) AS security_definers_have_no_public_execute
\gset
\if :database_public_safe
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'PUBLIC has CREATE or TEMPORARY on the destination database'; END $failure$;
\endif
\if :schemas_have_no_public_create
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'PUBLIC has CREATE on a non-system schema'; END $failure$;
\endif
\if :relations_have_no_public_grants
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'PUBLIC has a grant on a non-system relation'; END $failure$;
\endif
\if :columns_have_no_public_grants
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'PUBLIC has a column grant in a non-system schema'; END $failure$;
\endif
\if :security_definers_have_no_public_execute
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'PUBLIC can execute a non-system SECURITY DEFINER routine'; END $failure$;
\endif

\if :install_mode
  \ir install.sql
\else
  \ir upgrade.sql
\endif

\if :target_v2
  \ir migrations/002-views.sql
\endif
\ir privileges.sql

COMMIT;
