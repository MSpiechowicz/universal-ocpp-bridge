-- Refuse to adopt a role, schema, or version whose ownership or privileges are not known.
SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'runtime_role') AS role_exists,
       EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = :'schema_name') AS schema_exists,
       EXISTS (
         SELECT 1
         FROM pg_roles AS role
         WHERE role.rolname = :'runtime_role'
           AND NOT role.rolsuper
           AND NOT role.rolcreatedb
           AND NOT role.rolcreaterole
           AND NOT role.rolinherit
           AND NOT role.rolreplication
           AND NOT role.rolbypassrls
       ) AS role_is_restricted,
       EXISTS (
         SELECT 1
         FROM pg_namespace AS namespace
         WHERE namespace.nspname = :'schema_name'
           AND namespace.nspowner = (SELECT oid FROM pg_roles WHERE rolname = current_user)
       ) AS schema_has_admin_owner
\gset
\if :role_exists
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role does not exist'; END $failure$;
\endif
\if :schema_exists
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'managed schema does not exist'; END $failure$;
\endif
\if :role_is_restricted
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role has an unsafe attribute'; END $failure$;
\endif
\if :schema_has_admin_owner
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'managed schema is not owned by the provisioning administrator'; END $failure$;
\endif

SELECT NOT EXISTS (
  SELECT 1
  FROM pg_auth_members AS membership
  WHERE membership.member = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_has_no_memberships,
NOT EXISTS (
  SELECT 1
  FROM pg_database AS database
  WHERE database.datname = current_database()
    AND database.datdba = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_owns_no_database,
NOT EXISTS (
  SELECT 1
  FROM pg_namespace AS namespace
  WHERE namespace.nspowner = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_owns_no_schema,
NOT EXISTS (
  SELECT 1
  FROM pg_class AS relation
  WHERE relation.relowner = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_owns_no_relation,
NOT EXISTS (
  SELECT 1
  FROM pg_type AS type
  WHERE type.typowner = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_owns_no_type,
NOT EXISTS (
  SELECT 1
  FROM pg_proc AS routine
  WHERE routine.proowner = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_owns_no_routine
\gset
\if :role_has_no_memberships
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role has membership grants'; END $failure$;
\endif
\if :role_owns_no_database
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role owns the destination database'; END $failure$;
\endif
\if :role_owns_no_schema
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role owns a schema'; END $failure$;
\endif
\if :role_owns_no_relation
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role owns a relation'; END $failure$;
\endif
\if :role_owns_no_type
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role owns a type'; END $failure$;
\endif
\if :role_owns_no_routine
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role owns a routine'; END $failure$;
\endif

SELECT NOT EXISTS (
  SELECT 1
  FROM pg_database AS database
  CROSS JOIN LATERAL aclexplode(COALESCE(database.datacl, acldefault('d', database.datdba))) AS grant_item
  WHERE database.datname = current_database()
    AND grant_item.grantee = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
    AND grant_item.privilege_type <> 'CONNECT'
) AS role_has_only_connect_database_grant,
NOT EXISTS (
  SELECT 1
  FROM pg_namespace AS namespace
  CROSS JOIN LATERAL aclexplode(COALESCE(namespace.nspacl, acldefault('n', namespace.nspowner))) AS grant_item
  WHERE namespace.nspname <> :'schema_name'
    AND grant_item.grantee = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_has_no_other_schema_grants,
NOT EXISTS (
  SELECT 1
  FROM pg_class AS relation
  JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
  CROSS JOIN LATERAL aclexplode(COALESCE(relation.relacl, acldefault('r', relation.relowner))) AS grant_item
  WHERE namespace.nspname <> :'schema_name'
    AND grant_item.grantee = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_has_no_other_relation_grants,
NOT EXISTS (
  SELECT 1
  FROM pg_attribute AS attribute
  JOIN pg_class AS relation ON relation.oid = attribute.attrelid
  JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
  CROSS JOIN LATERAL aclexplode(attribute.attacl) AS grant_item
  WHERE attribute.attnum > 0
    AND NOT attribute.attisdropped
    AND namespace.nspname <> :'schema_name'
    AND grant_item.grantee = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_has_no_other_column_grants,
NOT EXISTS (
  SELECT 1
  FROM pg_proc AS routine
  JOIN pg_namespace AS namespace ON namespace.oid = routine.pronamespace
  CROSS JOIN LATERAL aclexplode(COALESCE(routine.proacl, acldefault('f', routine.proowner))) AS grant_item
  WHERE namespace.nspname <> :'schema_name'
    AND grant_item.grantee = (SELECT oid FROM pg_roles WHERE rolname = :'runtime_role')
) AS role_has_no_other_routine_grants
\gset
\if :role_has_only_connect_database_grant
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role has a direct database grant beyond CONNECT'; END $failure$;
\endif
\if :role_has_no_other_schema_grants
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role has a grant outside the managed schema'; END $failure$;
\endif
\if :role_has_no_other_relation_grants
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role has a relation grant outside the managed schema'; END $failure$;
\endif
\if :role_has_no_other_column_grants
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role has a column grant outside the managed schema'; END $failure$;
\endif
\if :role_has_no_other_routine_grants
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role has a routine grant outside the managed schema'; END $failure$;
\endif

SELECT EXISTS (
  SELECT 1
  FROM pg_class AS relation
  JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
  WHERE namespace.nspname = :'schema_name'
    AND relation.relname = 'schema_version'
    AND relation.relkind = 'r'
) AS version_table_exists,
EXISTS (
  SELECT 1
  FROM pg_class AS relation
  JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
  WHERE namespace.nspname = :'schema_name'
    AND relation.relname = 'canonical_events'
    AND relation.relkind = 'r'
) AS canonical_events_exists,
NOT EXISTS (
  SELECT 1
  FROM pg_class AS relation
  JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
  CROSS JOIN LATERAL aclexplode(COALESCE(relation.relacl, acldefault('r', relation.relowner))) AS grant_item
  WHERE namespace.nspname = :'schema_name'
    AND grant_item.grantee = 0
) AS managed_relations_have_no_public_grants,
NOT EXISTS (
  SELECT 1
  FROM pg_proc AS routine
  JOIN pg_namespace AS namespace ON namespace.oid = routine.pronamespace
  WHERE namespace.nspname = :'schema_name'
) AS managed_schema_has_no_routines
\gset
\if :version_table_exists
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'managed schema has no schema_version table'; END $failure$;
\endif
\if :canonical_events_exists
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'managed schema has no canonical_events table'; END $failure$;
\endif
\if :managed_relations_have_no_public_grants
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'PUBLIC has a grant on a managed relation'; END $failure$;
\endif
\if :managed_schema_has_no_routines
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'managed schema contains an unknown routine'; END $failure$;
\endif

SELECT set_config('search_path', format('%I, pg_catalog', :'schema_name'), true);
SELECT count(*) = 1 AS has_one_version FROM schema_version
\gset
\if :has_one_version
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'schema_version must contain exactly one row'; END $failure$;
\endif
SELECT version AS installed_version, runtime_role AS provisioned_runtime_role FROM schema_version
\gset
SELECT (:'installed_version'::integer BETWEEN 1 AND 2
        AND :'installed_version'::integer <= :'target_version'::integer) AS installed_version_is_supported
\gset
\if :installed_version_is_supported
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'managed schema has an unsupported or future version'; END $failure$;
\endif
SELECT (:'provisioned_runtime_role' = :'runtime_role') AS provisioned_role_matches
\gset
\if :provisioned_role_matches
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'runtime role does not match the managed schema metadata'; END $failure$;
\endif

SELECT NOT EXISTS (
  SELECT 1
  FROM pg_class AS relation
  JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
  WHERE namespace.nspname = :'schema_name'
    AND relation.relkind IN ('r', 'p', 'v', 'm', 'S', 'i', 'f')
    AND relation.relname <> ALL (
      CASE WHEN :'installed_version'::integer = 1 THEN ARRAY[
        'schema_version', 'schema_version_pkey', 'canonical_events', 'canonical_events_identity_key'
      ] ELSE ARRAY[
        'schema_version', 'schema_version_pkey', 'canonical_events', 'canonical_events_identity_key',
        'canonical_events_resource_idx', 'canonical_events_source_time_idx',
        'canonical_events_observed_at_idx', 'canonical_events_event_type_idx',
        'measurements', 'transactions', 'command_results'
      ] END
    )
) AS managed_relations_are_known,
NOT EXISTS (
  SELECT 1
  FROM pg_class AS relation
  JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
  WHERE namespace.nspname = :'schema_name'
    AND relation.relowner <> (SELECT oid FROM pg_roles WHERE rolname = current_user)
) AS managed_relations_have_admin_owner
\gset
\if :managed_relations_are_known
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'managed schema contains an unknown relation'; END $failure$;
\endif
\if :managed_relations_have_admin_owner
\else
  DO $failure$ BEGIN RAISE EXCEPTION 'managed relation is not owned by the provisioning administrator'; END $failure$;
\endif
