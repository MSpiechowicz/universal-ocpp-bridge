#!/usr/bin/env bash
# Disposable Docker cluster only; no host ports, volumes, databases or credentials are used.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
readonly image='postgres:17@sha256:e42539a54ee3e82e21f19d7eded61b579869585f86b90b5e851ff0d3dd8b4001'
container_id="$(docker run --detach --rm --network none -e POSTGRES_HOST_AUTH_METHOD=trust "$image")"
trap 'docker rm --force "$container_id" >/dev/null' EXIT
ready=false
for _ in {1..60}; do
  if docker exec "$container_id" pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; then ready=true; break; fi
  sleep 1
done
[[ "$ready" == true ]]
psql_admin() { docker exec -i "$container_id" psql -X -v ON_ERROR_STOP=1 -U postgres "$@"; }
psql_admin <<'SQL'
CREATE DATABASE production;
\connect production
CREATE TABLE public.production_canary (value text);
INSERT INTO public.production_canary VALUES ('untouched');
-- Deliberately generous object defaults: the staging HBA fence must still deny access.
GRANT ALL ON public.production_canary TO PUBLIC;
SQL
psql_admin < packaging/postgresql/staging.sql
# Ephemeral TLS material for testing the packaged hostssl rules, with no host key files.
docker exec -u postgres "$container_id" sh -c 'openssl req -new -x509 -nodes -newkey rsa:2048 -days 1 -subj /CN=localhost -addext "subjectAltName=IP:127.0.0.1,DNS:localhost" -keyout "$PGDATA/server.key" -out "$PGDATA/server.crt" >/dev/null 2>&1; chmod 600 "$PGDATA/server.key"'
psql_admin -c "ALTER SYSTEM SET ssl = 'on'"
psql_admin -c 'SELECT pg_reload_conf()'
# Install the actual packaged rules ahead of initialization's broad local trust rules.
docker cp packaging/postgresql/staging.pg_hba.conf "$container_id:/tmp/staging.hba" >/dev/null
docker exec -u postgres "$container_id" sh -c 'cat /tmp/staging.hba "$PGDATA/pg_hba.conf" > "$PGDATA/hba.new"; mv "$PGDATA/hba.new" "$PGDATA/pg_hba.conf"'
# Fresh disposable credential, not copied from production or stored in the repository.
stage_password="$(cat /proc/sys/kernel/random/uuid)"
psql_admin --set "stage_password=$stage_password" <<'SQL'
SELECT pg_reload_conf();
SELECT format('ALTER ROLE uob_staging LOGIN PASSWORD %L', :'stage_password') \gexec
SQL
stage() { docker exec -e "PGPASSWORD=$stage_password" -i "$container_id" psql -X -v ON_ERROR_STOP=1 -U uob_staging "$@"; }
[[ "$(psql_admin -Atc 'SELECT count(*) FROM pg_hba_file_rules WHERE error IS NOT NULL')" == 0 ]]
[[ "$(psql_admin -h 127.0.0.1 -d production -Atc 'SELECT value FROM production_canary')" == untouched ]]
stage -d uob_staging <<'SQL'
INSERT INTO staging.observations VALUES ('staging', '00000000-0000-4000-8000-000000000001', 0, 0, 'Available');
SELECT * FROM staging.observations;
SQL
reject() {
  if "$@" >/dev/null 2>&1; then echo 'isolation assertion unexpectedly succeeded' >&2; exit 1; fi
}
# Actual staging credentials cannot read/write even PUBLIC-granted production tables.
connection_denied() {
  local failure
  if failure="$(stage "$@" 2>&1)"; then echo 'forbidden connection succeeded' >&2; exit 1; fi
  [[ "$failure" == *'pg_hba.conf rejects connection'* ]]
}
connection_denied -d production -c 'SELECT * FROM public.production_canary'
connection_denied -d production -c "INSERT INTO public.production_canary VALUES ('corrupted')"
connection_denied -d postgres -c 'SELECT 1'
connection_denied -d template1 -c 'SELECT 1'
connection_denied -h 127.0.0.1 -d production -c 'SELECT * FROM public.production_canary'
connection_denied -d 'host=127.0.0.1 dbname=uob_staging sslmode=disable' -c 'SELECT 1' # plaintext forbidden
stage -d 'host=127.0.0.1 dbname=uob_staging sslmode=verify-full sslrootcert=/var/lib/postgresql/data/server.crt' -c 'SELECT count(*) FROM staging.observations'
reject stage -d uob_staging -c 'SET ROLE postgres'
reject stage -d uob_staging -c 'CREATE DATABASE escaped'
reject stage -d uob_staging -c 'CREATE TABLE staging.escaped (x int)'
reject stage -d uob_staging -c "INSERT INTO staging.observations VALUES ('production', '00000000-0000-4000-8000-000000000002', 0, 0, 'Available')"
reject stage -d uob_staging -c 'DELETE FROM staging.observations'
reject docker exec -e PGPASSWORD=wrong -i "$container_id" psql -X -v ON_ERROR_STOP=1 -U uob_staging -d uob_staging -c 'SELECT 1'
# Provisioning refuses to adopt an existing role/database.
reject psql_admin -f /dev/stdin < packaging/postgresql/staging.sql
[[ "$(psql_admin -d production -Atc 'SELECT string_agg(value, '\''|'\'') FROM production_canary')" == untouched ]]
[[ "$(psql_admin -Atc "SELECT count(*) FROM pg_auth_members WHERE member = (SELECT oid FROM pg_roles WHERE rolname='uob_staging')")" == 0 ]]
echo 'staging PostgreSQL isolation passed'
