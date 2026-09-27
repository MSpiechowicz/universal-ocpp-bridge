#!/usr/bin/env bash
# Only disposable PostgreSQL and loopback ports. Never points at an existing database.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
image='postgres:17@sha256:e42539a54ee3e82e21f19d7eded61b579869585f86b90b5e851ff0d3dd8b4001'
for executable in docker openssl python3 cargo; do
  command -v "$executable" >/dev/null || { echo "missing $executable" >&2; exit 1; }
done
scratch="$(mktemp -d)"
chmod 700 "$scratch"
container=''
proxy=''
cleanup() {
  if [[ -n "$proxy" ]]; then kill "$proxy" 2>/dev/null || :; wait "$proxy" 2>/dev/null || :; fi
  if [[ -n "$container" ]]; then docker rm --force "$container" >/dev/null 2>&1 || :; fi
  rm -rf -- "$scratch"
}
trap cleanup EXIT
password="$(cat /proc/sys/kernel/random/uuid)"
client_password="$(cat /proc/sys/kernel/random/uuid)"
openssl req -new -x509 -nodes -newkey rsa:2048 -sha256 -days 1 -subj '/CN=qualification CA' \
  -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -keyout "$scratch/ca.key" -out "$scratch/ca.crt" >/dev/null 2>&1
openssl req -new -nodes -newkey rsa:2048 -sha256 -subj '/CN=localhost' \
  -keyout "$scratch/server.key" -out "$scratch/server.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n' > "$scratch/server.ext"
openssl x509 -req -in "$scratch/server.csr" -CA "$scratch/ca.crt" -CAkey "$scratch/ca.key" \
  -CAcreateserial -days 1 -sha256 -extfile "$scratch/server.ext" -out "$scratch/server.crt" >/dev/null 2>&1
printf '[ca]\ndefault_ca=issuer\n[issuer]\ndatabase=%s/index\nserial=%s/serial\nnew_certs_dir=%s\nprivate_key=%s/ca.key\ncertificate=%s/ca.crt\ndefault_md=sha256\npolicy=policy\nx509_extensions=server_ext\n[policy]\ncommonName=supplied\n[server_ext]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n' \
  "$scratch" "$scratch" "$scratch" "$scratch" "$scratch" > "$scratch/ca.conf"
: > "$scratch/index"
printf '1000\n' > "$scratch/serial"
openssl ca -batch -config "$scratch/ca.conf" -startdate 20000101000000Z -enddate 20010101000000Z \
  -in "$scratch/server.csr" -out "$scratch/expired.crt" >/dev/null 2>&1
: > "$scratch/index"
openssl ca -batch -config "$scratch/ca.conf" -startdate 20400101000000Z -enddate 20410101000000Z \
  -in "$scratch/server.csr" -out "$scratch/future.crt" >/dev/null 2>&1
openssl req -new -x509 -nodes -newkey rsa:2048 -sha256 -days 1 -subj '/CN=untrusted' \
  -keyout "$scratch/untrusted.key" -out "$scratch/untrusted.crt" >/dev/null 2>&1
chmod 600 "$scratch"/*.key
container="$(docker run --detach --rm --network bridge -p 127.0.0.1::5432 \
  -e POSTGRES_PASSWORD="$password" -e POSTGRES_HOST_AUTH_METHOD=scram-sha-256 "$image")"
ready=false
for _ in {1..60}; do
  if docker exec "$container" pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; then ready=true; break; fi
  sleep 1
done
[[ "$ready" == true ]] || { echo 'disposable PostgreSQL did not start' >&2; exit 1; }
docker cp "$scratch/server.key" "$container:/var/lib/postgresql/data/server.key" >/dev/null
docker cp "$scratch/server.crt" "$container:/var/lib/postgresql/data/server.crt" >/dev/null
docker exec "$container" chown postgres:postgres /var/lib/postgresql/data/server.key /var/lib/postgresql/data/server.crt
docker exec "$container" chmod 600 /var/lib/postgresql/data/server.key
docker exec -e PGPASSWORD="$password" "$container" psql -X -v ON_ERROR_STOP=1 -U postgres \
  -c "ALTER SYSTEM SET ssl = 'on'" >/dev/null
docker exec -e PGPASSWORD="$password" "$container" psql -X -v ON_ERROR_STOP=1 -U postgres \
  -c 'SELECT pg_reload_conf()' >/dev/null
# A dedicated SCRAM login has only the table rights needed for the fixed probe.
docker exec -i -e PGPASSWORD="$password" "$container" \
  psql -X -v ON_ERROR_STOP=1 -v "client_password=$client_password" -U postgres >/dev/null <<'SQL'
SELECT format('CREATE ROLE uob_qualification LOGIN PASSWORD %L', :'client_password') \gexec
CREATE TABLE public.qualification_markers (marker text PRIMARY KEY);
GRANT INSERT, SELECT ON public.qualification_markers TO uob_qualification;
SQL
scram_ready="$(docker exec -e PGPASSWORD="$password" "$container" psql -X -At -U postgres \
  -c "SELECT (SELECT rolpassword LIKE 'SCRAM-SHA-256$%' FROM pg_authid WHERE rolname = 'uob_qualification') AND EXISTS (SELECT 1 FROM pg_hba_file_rules WHERE type = 'host' AND auth_method = 'scram-sha-256')")"
[[ "$scram_ready" == t ]] || { echo 'disposable SCRAM authentication not configured' >&2; exit 1; }
endpoint="$(docker port "$container" 5432/tcp)"
[[ "$endpoint" == 127.0.0.1:* ]] || { echo 'database port not loopback-only' >&2; exit 1; }
printf 'username = "uob_qualification"\npassword = "%s"\nca_certificate_file = "%s/ca.crt"\n' "$client_password" "$scratch" > "$scratch/credential.toml"
chmod 600 "$scratch/credential.toml"
printf 'username = "uob_qualification"\npassword = "%s"\nca_certificate_file = "%s/untrusted.crt"\n' "$client_password" "$scratch" > "$scratch/bad.toml"
chmod 600 "$scratch/bad.toml"
printf 'username = "uob_qualification"\npassword = "%s"\n' "$client_password" > "$scratch/plain.toml"
chmod 600 "$scratch/plain.toml"
printf 'username = "uob_qualification"\npassword = "%s"\nca_certificate_file = "%s/malformed.crt"\n' "$client_password" "$scratch" > "$scratch/malformed.toml"
printf 'not a PEM certificate\n' > "$scratch/malformed.crt"
chmod 600 "$scratch/malformed.toml"
cargo build --locked --release -p uob-postgresql-export-adapter
binary=target/release/uob-postgresql-export-adapter
reject_secret() {
  [[ "$1" != *"$password"* && "$1" != *"$client_password"* ]] || { echo 'secret in adapter output' >&2; exit 1; }
}
expect_ok() {
  local output status=0
  output="$("$binary" "$@" 2>&1)" || status=$?
  reject_secret "$output"
  [[ "$status" == 0 ]] || { echo "expected success: $output" >&2; exit 1; }
  [[ "$output" == *'outcome=ok'* && "$output" == *'driver_tasks=0'* ]] || { echo "unexpected result: $output" >&2; exit 1; }
  echo "$output"
}
expect_error() {
  local category="$1" output status=0
  shift
  output="$("$binary" "$@" 2>&1)" || status=$?
  reject_secret "$output"
  [[ "$status" != 0 ]] || { echo "unexpected success: $output" >&2; exit 1; }
  [[ "$output" == *"retry=$category"* && "$output" == *'driver_tasks=0'* ]] || { echo "unexpected result: $output" >&2; exit 1; }
  echo "$output"
}
expect_bounded_failure() {
  local output
  # A regression must fail the test, not let a 2 GiB advertised frame exhaust the host.
  output="$( (ulimit -v 262144; expect_error Retryable "$@") )"
  [[ "$output" == *'context=postgres.connect.setup'* && "$output" =~ elapsed_ms=([0-9]+) ]] \
    || { echo "unbounded frame did not fail closed: $output" >&2; exit 1; }
  (( ${BASH_REMATCH[1]} < 2000 )) \
    || { echo "frame rejection waited for connection deadline: $output" >&2; exit 1; }
  echo "$output"
}
expect_ok probe production verified "$endpoint" localhost postgres "$scratch/credential.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch/bad.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch/malformed.toml"
expect_error Permanent probe production verified "$endpoint" wrong.local postgres "$scratch/credential.toml"
expect_error Permanent probe production plaintext "$endpoint" localhost postgres "$scratch/plain.toml"
expect_error Permanent probe staging plaintext "$endpoint" localhost postgres "$scratch/plain.toml"
expect_ok probe demo-isolated plaintext "$endpoint" localhost postgres "$scratch/plain.toml"
output="$(expect_error Permanent probe demo-isolated plaintext 192.0.2.1:5432 localhost postgres "$scratch/missing.toml")"
[[ "$output" == *'context=postgres.transport.invalid'* ]] || { echo "plaintext endpoint checked after credentials/network: $output" >&2; exit 1; }
chmod 644 "$scratch/credential.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch/credential.toml"
chmod 600 "$scratch/credential.toml"
ln -s credential.toml "$scratch/link.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch/link.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch/missing.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch"
mkfifo "$scratch/pipe.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch/pipe.toml"
printf '%65537s' ' ' > "$scratch/oversized.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch/oversized.toml"
printf 'username = ' > "$scratch/invalid.toml"
expect_error Permanent probe production verified "$endpoint" localhost postgres "$scratch/invalid.toml"
expect_error Retryable sleep production verified "$endpoint" localhost postgres "$scratch/credential.toml"
expect_error Retryable cancel production verified "$endpoint" localhost postgres "$scratch/credential.toml"
expect_ok transaction production verified "$endpoint" localhost postgres "$scratch/credential.toml" committed_once
marker_count="$(docker exec -e PGPASSWORD="$password" "$container" psql -X -At -U postgres -c "SELECT count(*) FROM public.qualification_markers WHERE marker = 'committed_once'")"
[[ "$marker_count" == 1 ]]
start_proxy() {
  : > "$scratch/proxy.log"
  python3 scripts/postgresql-client-fixture.py "$1" "$endpoint" "${2:-$scratch/server.crt}" "$scratch/server.key" "$scratch/ca.crt" \
    --cycles "${3:-1}" > "$scratch/proxy.log" 2>&1 &
  proxy=$!
  local line
  for _ in {1..100}; do
    if [[ -s "$scratch/proxy.log" ]]; then
      IFS= read -r line < "$scratch/proxy.log"
      if [[ "$line" == port=* ]]; then proxy_endpoint="127.0.0.1:${line#port=}"; return; fi
    fi
    sleep 0.05
  done
  echo 'fault proxy failed to start' >&2; exit 1
}
stop_proxy() { wait "$proxy" || :; proxy=''; }
start_proxy auth-stall "$scratch/expired.crt"
expect_error Permanent probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
start_proxy auth-stall "$scratch/future.crt"
expect_error Permanent probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
start_proxy refuse
expect_error Permanent probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
start_proxy stall
expect_error Retryable probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
start_proxy tls-stall
expect_error Retryable probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
start_proxy auth-stall
expect_error Retryable probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
start_proxy startup-reset
expect_error Retryable probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
[[ "$(cat "$scratch/proxy.log")" == *'startup_reset=1'* ]]
expect_ok probe production verified "$endpoint" localhost postgres "$scratch/credential.toml"
start_proxy startup-close
expect_error Retryable probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
[[ "$(cat "$scratch/proxy.log")" == *'startup_closed=1'* ]]
expect_ok probe production verified "$endpoint" localhost postgres "$scratch/credential.toml"
start_proxy hostile-header
expect_bounded_failure probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
[[ "$(cat "$scratch/proxy.log")" == *'hostile_sent=hostile-header'* && "$(cat "$scratch/proxy.log")" == *'client_closed=1'* ]]
expect_ok probe production verified "$endpoint" localhost postgres "$scratch/credential.toml"
start_proxy hostile-aggregate
expect_bounded_failure probe production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
[[ "$(cat "$scratch/proxy.log")" == *'hostile_sent=hostile-aggregate'* && "$(cat "$scratch/proxy.log")" == *'client_closed=1'* ]]
expect_ok probe production verified "$endpoint" localhost postgres "$scratch/credential.toml"
start_proxy before-commit-loss
expect_error Uncertain transaction production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml" rolled_back
stop_proxy
[[ "$(cat "$scratch/proxy.log")" == *'commit_not_forwarded=1'* ]]
marker_count="$(docker exec -e PGPASSWORD="$password" "$container" psql -X -At -U postgres -c "SELECT count(*) FROM public.qualification_markers WHERE marker = 'rolled_back'")"
[[ "$marker_count" == 0 ]]
start_proxy commit-loss
expect_error Uncertain transaction production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml" committed_without_ack
stop_proxy
# The gate logs a port first; require its commit barrier on any line.
[[ "$(cat "$scratch/proxy.log")" == *'commit_ack_suppressed=1'* ]]
marker_count="$(docker exec -e PGPASSWORD="$password" "$container" psql -X -At -U postgres -c "SELECT count(*) FROM public.qualification_markers WHERE marker = 'committed_without_ack'")"
[[ "$marker_count" == 1 ]]
start_proxy insert-loss
expect_error Uncertain transaction production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml" insert_ack_lost
stop_proxy
[[ "$(cat "$scratch/proxy.log")" == *'insert_submitted=1'* ]]
expect_stress() {
  local iterations="$1" output
  shift
  output="$(expect_ok "$@")"
  [[ "$output" == *"iterations=$iterations "* && "$output" == *'peak_sockets=1'* ]] || {
    echo "missing measured resource workload: $output" >&2; exit 1;
  }
  echo "$output"
}
expect_stress 100 stress production verified "$endpoint" localhost postgres "$scratch/credential.toml"
start_proxy recovery "$scratch/server.crt" 1000
expect_stress 1000 stress-recovery production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
[[ "$(cat "$scratch/proxy.log")" == *'completed_cycles=1000'* ]]
expect_stress 100 stress-cancel production verified "$endpoint" localhost postgres "$scratch/credential.toml"
start_proxy before-commit-loss "$scratch/server.crt" 100
expect_stress 100 stress-rollback production verified "$proxy_endpoint" localhost postgres "$scratch/credential.toml"
stop_proxy
[[ "$(cat "$scratch/proxy.log")" == *'completed_cycles=100'* ]]
marker_count="$(docker exec -e PGPASSWORD="$password" "$container" psql -X -At -U postgres -c "SELECT count(*) FROM public.qualification_markers WHERE marker LIKE 'rollback_%'")"
[[ "$marker_count" == 0 ]]
echo 'disposable PostgreSQL client qualification passed'
