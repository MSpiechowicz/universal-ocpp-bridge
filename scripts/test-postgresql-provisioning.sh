#!/usr/bin/env bash
# Exercises the shipped provisioning command in an isolated PostgreSQL 17 cluster.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

readonly image='postgres:17@sha256:e42539a54ee3e82e21f19d7eded61b579869585f86b90b5e851ff0d3dd8b4001'
readonly schema='uob_export'
readonly runtime='uob_export_runtime'
readonly v1_runtime='uob_export_v1_runtime'

container_id="$(docker run --detach --rm --network none -e POSTGRES_HOST_AUTH_METHOD=trust "$image")"
trap 'docker rm --force "$container_id" >/dev/null' EXIT

for _ in {1..60}; do
    if docker exec "$container_id" pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
docker exec "$container_id" pg_isready -h 127.0.0.1 -U postgres >/dev/null

docker exec "$container_id" mkdir -p /opt/uob/scripts /opt/uob/packaging/postgresql
# The container receives the actual command and migrations; it never mounts host source or data.
docker cp scripts/provision-postgresql.sh "$container_id:/opt/uob/scripts/provision-postgresql.sh" >/dev/null
docker cp packaging/postgresql/export "$container_id:/opt/uob/packaging/postgresql/export" >/dev/null
docker cp crates/contracts/tests/fixtures/export-batch-v1.json "$container_id:/tmp/export-batch-v1.json" >/dev/null
docker cp crates/contracts/tests/fixtures/command-results-v1.json "$container_id:/tmp/command-results-v1.json" >/dev/null

admin() {
    local database=$1
    shift
    docker exec -i -e "PGDATABASE=$database" "$container_id" \
        psql -X -v ON_ERROR_STOP=1 -U postgres "$@"
}
runtime_psql() {
    local database=$1 role=$2
    shift 2
    docker exec -i -e "PGDATABASE=$database" "$container_id" \
        psql -X -v ON_ERROR_STOP=1 -U "$role" "$@"
}
runtime_assert_sql() {
    local database=$1 role=$2 expected=$3 query=$4 description=$5
    assert_eq "$expected" "$(runtime_psql "$database" "$role" -Atc "$query")" "$description"
}
provision() {
    local database=$1 mode=$2 target_schema=$3 role=$4
    docker exec -i -e PGUSER=postgres -e "PGDATABASE=$database" "$container_id" \
        /opt/uob/scripts/provision-postgresql.sh "$mode" "$target_schema" "$role"
}
assert_eq() {
    local expected=$1 actual=$2 description=$3
    if [[ "$actual" != "$expected" ]]; then
        printf 'assertion failed: %s\nexpected: %s\nactual: %s\n' \
            "$description" "$expected" "$actual" >&2
        exit 1
    fi
}
assert_sql() {
    local database=$1 expected=$2 query=$3 description=$4
    assert_eq "$expected" "$(admin "$database" -Atc "$query")" "$description"
}
reject() {
    if "$@" >/dev/null 2>&1; then
        printf 'forbidden operation unexpectedly succeeded: %q\n' "$*" >&2
        exit 1
    fi
}
create_database() {
    local database=$1
    admin postgres -c "CREATE DATABASE \"$database\""
    admin "$database" <<SQL
REVOKE CREATE, TEMPORARY ON DATABASE "$database" FROM PUBLIC;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
CREATE SCHEMA private;
CREATE TABLE private.unrelated (secret text NOT NULL);
INSERT INTO private.unrelated VALUES ('not-for-runtime');
SQL
}
install_v1() {
    local database=$1 target_schema=$2 role=$3
    docker exec -i -e "PGDATABASE=$database" "$container_id" psql -X -v ON_ERROR_STOP=1 \
        -U postgres -v provision_mode=install -v "schema_name=$target_schema" \
        -v "runtime_role=$role" -v target_version=1 \
        -f /opt/uob/packaging/postgresql/export/provision.sql
}
insert_export_records() {
    local database=$1 target_schema=$2
    admin "$database" <<SQL
INSERT INTO "$target_schema".canonical_events (canonical_record)
SELECT record
FROM jsonb_array_elements((pg_read_file('/tmp/export-batch-v1.json')::jsonb)->'records') AS record;

INSERT INTO "$target_schema".canonical_events (canonical_record)
SELECT jsonb_set(jsonb_set(record, '{metadata,identity}',
    '{"record_id":"transaction-record-500"}'::jsonb), '{payload}',
    jsonb_build_object('kind', 'transaction_lifecycle', 'data', jsonb_build_object(
        'transaction_id', 'tx-500',
        'resource', record #> '{metadata,resource}',
        'state', 'active',
        'started_at', '2026-09-02T01:02:03.004Z'
    )))
FROM (SELECT (pg_read_file('/tmp/export-batch-v1.json')::jsonb)->'records'->0 AS record) AS source;

INSERT INTO "$target_schema".canonical_events (canonical_record)
SELECT jsonb_set(jsonb_set(record, '{metadata,identity}',
    '{"record_id":"command-record-700"}'::jsonb), '{payload}',
    jsonb_build_object('kind', 'command_result',
        'data', (pg_read_file('/tmp/command-results-v1.json')::jsonb)->5))
FROM (SELECT (pg_read_file('/tmp/export-batch-v1.json')::jsonb)->'records'->0 AS record) AS source;
SQL
}
assert_layout() {
    local database=$1 target_schema=$2 role=$3
    assert_sql "$database" '1' "SELECT count(*) FROM pg_indexes WHERE schemaname = '$target_schema' AND tablename = 'canonical_events' AND indexname = 'canonical_events_identity_key'" 'identity uniqueness index exists'
    assert_sql "$database" '1' "SELECT count(*) FROM pg_indexes WHERE schemaname = '$target_schema' AND tablename = 'canonical_events' AND indexname = 'canonical_events_resource_idx'" 'resource GIN index exists'
    assert_sql "$database" '1' "SELECT count(*) FROM pg_indexes WHERE schemaname = '$target_schema' AND tablename = 'canonical_events' AND indexname = 'canonical_events_source_time_idx'" 'source time index exists'
    assert_sql "$database" '1' "SELECT count(*) FROM pg_indexes WHERE schemaname = '$target_schema' AND tablename = 'canonical_events' AND indexname = 'canonical_events_observed_at_idx'" 'observed-at index exists'
    assert_sql "$database" '1' "SELECT count(*) FROM pg_indexes WHERE schemaname = '$target_schema' AND tablename = 'canonical_events' AND indexname = 'canonical_events_event_type_idx'" 'event type index exists'
    assert_sql "$database" '3' "SELECT count(*) FROM pg_views WHERE schemaname = '$target_schema' AND viewname IN ('measurements', 'transactions', 'command_results')" 'all preserving views exist'
    assert_sql "$database" 'f|f|f|f|f|f|f' "SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolinherit, rolreplication, rolbypassrls FROM pg_roles WHERE rolname = '$role'" 'runtime role is least privileged'
    assert_sql "$database" '0' "SELECT count(*) FROM pg_auth_members WHERE member = (SELECT oid FROM pg_roles WHERE rolname = '$role')" 'runtime role has no memberships'
    assert_sql "$database" 'canonical_record' "SELECT column_name FROM information_schema.columns WHERE table_schema = '$target_schema' AND table_name = 'canonical_events' AND is_generated = 'NEVER'" 'canonical table has only the source record writable'
    assert_sql "$database" 't' "SELECT has_column_privilege('$role', '\"$target_schema\".canonical_events', 'canonical_record', 'INSERT') AND NOT has_column_privilege('$role', '\"$target_schema\".canonical_events', 'environment', 'INSERT')" 'runtime INSERT is limited to canonical_record'
}
assert_preservation() {
    local database=$1 target_schema=$2
    assert_sql "$database" '{"type": "decimal", "value": "12.34567890123456789"}|{"level": "uncertain", "reason": "device_clock_drift"}|W|2026-09-01T12:14:59.125Z|2026-09-01T12:15:00Z' "SELECT value::text || '|' || quality::text || '|' || unit || '|' || measurement_source_time || '|' || measurement_observed_at FROM \"$target_schema\".measurements WHERE record_id = 'event-meter-values-42'" 'measurement view preserves typed decimal, quality, unit, and times'
    assert_sql "$database" '2026-09-01T12:14:59.125Z|2026-09-01T12:15:00Z' "SELECT source_time || '|' || observed_at FROM \"$target_schema\".canonical_events WHERE record_id = 'event-meter-values-42' AND subrecord_id = 0" 'generated source and observation timestamps retain exact text'
    assert_sql "$database" 't' "SELECT payload = canonical_record #> '{payload,data}' AND canonical_record = (pg_read_file('/tmp/export-batch-v1.json')::jsonb)->'records'->0 AND payload #>> '{measurement,original_value}' = '12345.678901234567890' FROM \"$target_schema\".measurements WHERE record_id = 'event-meter-values-42'" 'measurement view preserves the complete source record and original decimal'
    assert_sql "$database" 't' "SELECT payload = canonical_record #> '{payload,data}' AND payload #>> '{state}' = 'active' AND payload #>> '{started_at}' = '2026-09-02T01:02:03.004Z' AND payload #> '{resource}' = canonical_record #> '{metadata,resource}' FROM \"$target_schema\".transactions WHERE record_id = 'transaction-record-500'" 'transaction view preserves lifecycle, resource, and timestamp'
    assert_sql "$database" 't' "SELECT payload = canonical_record #> '{payload,data}' AND payload = (pg_read_file('/tmp/command-results-v1.json')::jsonb)->5 AND payload #>> '{observed_effects,0,event_type}' = 'transaction.started.v1' FROM \"$target_schema\".command_results WHERE record_id = 'command-record-700'" 'command result view preserves response and separately observed effects'
}
assert_runtime_permissions() {
    local database=$1 target_schema=$2 role=$3
    admin "$database" -c "ALTER ROLE \"$role\" LOGIN"
    runtime_psql "$database" "$role" <<SQL
INSERT INTO "$target_schema".canonical_events (canonical_record)
SELECT jsonb_set(canonical_record, '{metadata,identity}', '{"record_id":"runtime-root"}')
FROM "$target_schema".canonical_events WHERE record_id = 'event-meter-values-42' AND subrecord_id = 0;
SQL
    # Missing and explicit-null subrecord IDs identify the same root.
    reject runtime_psql "$database" "$role" -c "INSERT INTO \"$target_schema\".canonical_events (canonical_record) SELECT jsonb_set(canonical_record, '{metadata,identity,subrecord_id}', 'null') FROM \"$target_schema\".canonical_events WHERE record_id = 'runtime-root'"
    runtime_psql "$database" "$role" <<SQL
INSERT INTO "$target_schema".canonical_events (canonical_record)
SELECT jsonb_set(canonical_record, '{metadata,runtime,environment}', '"staging"')
FROM "$target_schema".canonical_events WHERE record_id = 'runtime-root';
INSERT INTO "$target_schema".canonical_events (canonical_record)
SELECT jsonb_set(canonical_record, '{metadata,identity,subrecord_id}', '9')
FROM "$target_schema".canonical_events WHERE record_id = 'runtime-root' AND environment = 'production';
INSERT INTO "$target_schema".canonical_events (canonical_record)
SELECT jsonb_set(canonical_record, '{metadata,resource,bridge_id}', '"bridge-other"')
FROM "$target_schema".canonical_events WHERE record_id = 'runtime-root' AND environment = 'production' AND subrecord_id IS NULL;
SQL
    runtime_assert_sql "$database" "$role" '4' "SELECT count(*) FROM \"$target_schema\".canonical_events WHERE record_id = 'runtime-root'" 'identity separates environment, bridge, and child ordinal'
    # ON CONFLICT plus SELECT allows the future adapter to distinguish retry from corruption.
    assert_eq 'INSERT 0 0' "$(runtime_psql "$database" "$role" -c "INSERT INTO \"$target_schema\".canonical_events (canonical_record) SELECT canonical_record FROM \"$target_schema\".canonical_events WHERE record_id = 'command-record-700' ON CONFLICT ON CONSTRAINT canonical_events_identity_key DO NOTHING")" 'identical retry does not create or overwrite a record'
    assert_eq 'INSERT 0 0' "$(runtime_psql "$database" "$role" -c "INSERT INTO \"$target_schema\".canonical_events (canonical_record) SELECT jsonb_set(canonical_record, '{payload,data,lifecycle,accepted}', 'false') FROM \"$target_schema\".canonical_events WHERE record_id = 'command-record-700' ON CONFLICT ON CONSTRAINT canonical_events_identity_key DO NOTHING")" 'conflicting content cannot overwrite the accepted source result'
    reject runtime_psql "$database" "$role" -c "INSERT INTO \"$target_schema\".canonical_events (canonical_record) VALUES ('{}'::jsonb)"
    reject runtime_psql "$database" "$role" -c "INSERT INTO \"$target_schema\".canonical_events (canonical_record) SELECT jsonb_set(jsonb_set(canonical_record, '{metadata,identity,record_id}', '\"unknown-kind\"'), '{payload,kind}', '\"unknown_kind\"') FROM \"$target_schema\".canonical_events WHERE record_id = 'command-record-700'"
    runtime_assert_sql "$database" "$role" 'active' "SELECT payload->>'state' FROM \"$target_schema\".transactions WHERE record_id = 'transaction-record-500'" 'runtime can read transaction projection'
    runtime_assert_sql "$database" "$role" 'true' "SELECT payload #>> '{lifecycle,accepted}' FROM \"$target_schema\".command_results WHERE record_id = 'command-record-700'" 'runtime can read command projection'
    runtime_assert_sql "$database" "$role" '12.34567890123456789' "SELECT value->>'value' FROM \"$target_schema\".measurements WHERE record_id = 'event-meter-values-42'" 'runtime reads exact measurement projection'
    for sql in \
        "UPDATE \"$target_schema\".canonical_events SET canonical_record = canonical_record" \
        "DELETE FROM \"$target_schema\".canonical_events" \
        "TRUNCATE \"$target_schema\".canonical_events" \
        "ALTER TABLE \"$target_schema\".canonical_events ADD COLUMN escaped text" \
        "CREATE TABLE \"$target_schema\".escaped (value text)" \
        'CREATE TABLE public.escaped (value text)' \
        'CREATE TEMP TABLE escaped (value text)' \
        'CREATE SCHEMA escaped' \
        'CREATE ROLE escaped' \
        'SET ROLE postgres' \
        'SELECT * FROM private.unrelated' \
        "SELECT * FROM \"$target_schema\".schema_version"; do
        local failure
        if failure="$(runtime_psql "$database" "$role" -v VERBOSITY=verbose -c "$sql" 2>&1)"; then
            echo "runtime permission unexpectedly allowed: $sql" >&2
            exit 1
        fi
        [[ "$failure" == *'42501'* ]] || { echo "$failure" >&2; exit 1; }
    done
    # Upgrades must not require disabling a deployed runtime login or discard CONNECT.
    admin "$database" -c "GRANT CONNECT ON DATABASE \"$database\" TO \"$role\""
    provision "$database" upgrade "$target_schema" "$role"
    runtime_assert_sql "$database" "$role" '1' 'SELECT 1' 'upgraded runtime remains connectable'
}

create_database export_fresh
provision export_fresh install "$schema" "$runtime"
insert_export_records export_fresh "$schema"
assert_layout export_fresh "$schema" "$runtime"
assert_preservation export_fresh "$schema"
assert_runtime_permissions export_fresh "$schema" "$runtime"
provision export_fresh upgrade "$schema" "$runtime"
provision export_fresh upgrade "$schema" "$runtime"
reject provision export_fresh install "$schema" "$runtime"

# A populated v1 database must become v2 without copying or changing canonical rows.
create_database export_v1
install_v1 export_v1 "$schema" "$v1_runtime"
insert_export_records export_v1 "$schema"
assert_sql export_v1 '4' "SELECT count(*) FROM \"$schema\".canonical_events" 'v1 fixture is populated before upgrade'
provision export_v1 upgrade "$schema" "$v1_runtime"
assert_layout export_v1 "$schema" "$v1_runtime"
assert_preservation export_v1 "$schema"
provision export_v1 upgrade "$schema" "$v1_runtime"
assert_runtime_permissions export_v1 "$schema" "$v1_runtime"

# Unsafe preconditions must reject before a migration can modify version or ownership.
admin export_fresh -c 'CREATE ROLE export_escalation NOLOGIN'
admin export_fresh -c "GRANT export_escalation TO \"$runtime\""
reject provision export_fresh upgrade "$schema" "$runtime"
assert_sql export_fresh '2' "SELECT version FROM \"$schema\".schema_version" 'membership failure rolls back upgrade'
admin export_fresh -c "REVOKE export_escalation FROM \"$runtime\""
admin export_fresh -c "GRANT SELECT ON \"$schema\".canonical_events TO PUBLIC"
reject provision export_fresh upgrade "$schema" "$runtime"
assert_sql export_fresh '2' "SELECT version FROM \"$schema\".schema_version" 'PUBLIC table grant failure rolls back upgrade'
admin export_fresh -c "REVOKE SELECT ON \"$schema\".canonical_events FROM PUBLIC"
admin export_fresh -c 'CREATE ROLE export_intruder NOLOGIN'
admin export_fresh -c "ALTER SCHEMA \"$schema\" OWNER TO export_intruder"
reject provision export_fresh upgrade "$schema" "$runtime"
admin export_fresh -c "ALTER SCHEMA \"$schema\" OWNER TO postgres"
admin export_fresh -c "UPDATE \"$schema\".schema_version SET version = 3"
reject provision export_fresh upgrade "$schema" "$runtime"
assert_sql export_fresh '3' "SELECT version FROM \"$schema\".schema_version" 'future version remains untouched after rejection'
admin export_fresh -c "UPDATE \"$schema\".schema_version SET version = 2"

# A different existing role must never be adopted by an upgrade.
admin export_fresh -c 'CREATE ROLE wrong_runtime NOLOGIN NOINHERIT'
reject provision export_fresh upgrade "$schema" wrong_runtime
assert_sql export_fresh "$runtime" "SELECT runtime_role FROM \"$schema\".schema_version" 'upgrade preserves the bound runtime identity'

# PostgreSQL column ACLs survive table-level REVOKE unless explicitly removed.
admin export_fresh -c "GRANT UPDATE (canonical_record) ON \"$schema\".canonical_events TO \"$runtime\""
provision export_fresh upgrade "$schema" "$runtime"
reject runtime_psql export_fresh "$runtime" -c "UPDATE \"$schema\".canonical_events SET canonical_record = canonical_record"
admin export_fresh -c "GRANT SELECT (secret) ON private.unrelated TO \"$runtime\""
reject provision export_fresh upgrade "$schema" "$runtime"
admin export_fresh -c "REVOKE SELECT (secret) ON private.unrelated FROM \"$runtime\""

create_database export_existing
admin export_existing -c 'CREATE ROLE existing_runtime NOLOGIN'
reject provision export_existing install existing_schema existing_runtime
assert_sql export_existing '0' "SELECT count(*) FROM pg_namespace WHERE nspname = 'existing_schema'" 'existing role install creates no schema'
admin export_existing -c 'CREATE SCHEMA existing_schema'
reject provision export_existing install existing_schema distinct_runtime
assert_sql export_existing '0' "SELECT count(*) FROM pg_roles WHERE rolname = 'distinct_runtime'" 'existing schema install creates no role'

create_database export_unsafe
admin export_unsafe -c 'GRANT TEMPORARY ON DATABASE export_unsafe TO PUBLIC'
reject provision export_unsafe install unsafe_schema unsafe_runtime
assert_sql export_unsafe '0' "SELECT count(*) FROM pg_roles WHERE rolname = 'unsafe_runtime'" 'unsafe database grants do not create role'
assert_sql export_unsafe '0' "SELECT count(*) FROM pg_namespace WHERE nspname = 'unsafe_schema'" 'unsafe database grants do not create schema'
admin export_unsafe -c 'REVOKE TEMPORARY ON DATABASE export_unsafe FROM PUBLIC'
admin export_unsafe -c 'GRANT CREATE ON SCHEMA public TO PUBLIC'
reject provision export_unsafe install unsafe_schema unsafe_runtime
assert_sql export_unsafe '0' "SELECT count(*) FROM pg_roles WHERE rolname = 'unsafe_runtime'" 'unsafe schema grants do not create role'
admin export_unsafe -c 'REVOKE CREATE ON SCHEMA public FROM PUBLIC'
admin export_unsafe -c 'GRANT CREATE ON SCHEMA private TO PUBLIC'
reject provision export_unsafe install unsafe_schema unsafe_runtime
admin export_unsafe -c 'REVOKE CREATE ON SCHEMA private FROM PUBLIC'
admin export_unsafe -c 'GRANT SELECT ON private.unrelated TO PUBLIC'
reject provision export_unsafe install unsafe_schema unsafe_runtime
admin export_unsafe -c 'REVOKE SELECT ON private.unrelated FROM PUBLIC'
admin export_unsafe -c 'GRANT SELECT (secret) ON private.unrelated TO PUBLIC'
reject provision export_unsafe install unsafe_schema unsafe_runtime
admin export_unsafe -c 'REVOKE SELECT (secret) ON private.unrelated FROM PUBLIC'
admin export_unsafe -c "CREATE FUNCTION private.public_escape() RETURNS integer LANGUAGE sql SECURITY DEFINER AS 'SELECT 1'"
reject provision export_unsafe install unsafe_schema unsafe_runtime
admin export_unsafe -c 'REVOKE EXECUTE ON FUNCTION private.public_escape() FROM PUBLIC'
provision export_unsafe install unsafe_schema unsafe_runtime

# Identifier quoting and transaction rollback apply to roles as well as schema objects.
reject provision export_existing install pg_forbidden rollback_runtime
assert_sql export_existing '0' "SELECT count(*) FROM pg_roles WHERE rolname = 'rollback_runtime'" 'failed CREATE SCHEMA rolls back the preceding CREATE ROLE'
long_name="$(printf 'x%.0s' {1..64})"
reject provision export_existing install "$long_name" long_runtime
assert_sql export_existing '0' "SELECT count(*) FROM pg_roles WHERE rolname = 'long_runtime'" 'overlong identifiers cannot be silently truncated'
provision export_existing install 'export "quoted"' 'runtime "quoted"'
provision export_existing upgrade 'export "quoted"' 'runtime "quoted"'

# Existing administrator default grants must not leak into newly created export objects.
create_database export_defaults
admin export_defaults -c 'ALTER DEFAULT PRIVILEGES GRANT ALL ON TABLES TO PUBLIC'
provision export_defaults install "$schema" defaults_runtime
insert_export_records export_defaults "$schema"
assert_runtime_permissions export_defaults "$schema" defaults_runtime
assert_sql export_defaults '0' "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace CROSS JOIN LATERAL aclexplode(c.relacl) a WHERE n.nspname = '$schema' AND a.grantee = 0" 'fresh schema removes ambient default table grants'

echo 'PostgreSQL provisioning acceptance passed'
