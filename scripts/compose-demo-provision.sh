#!/usr/bin/env bash
# Materialize one disposable, private Compose instance. Never print credential contents.
set -euo pipefail
fail() { printf 'Compose provisioning: %s\n' "$*" >&2; exit 1; }
[[ $# == 3 ]] || fail 'usage: compose-demo-provision.sh RUN_DIRECTORY BRIDGE_ID mqtt|ems-http|ems-mqtt'
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workdir="$1" bridge="$2" mode="$3"
[[ "$workdir" == "$root"/target/compose-demo.* && ! -L "$workdir" && -d "$workdir" ]] || fail 'invalid private run directory'
[[ "$bridge" =~ ^site-[0-9a-f]{16}$ ]] || fail 'invalid bridge identity'
[[ "$mode" == mqtt || "$mode" == ems-http || "$mode" == ems-mqtt ]] || fail 'invalid target mode'
for tool in docker openssl sha256sum date; do
  command -v "$tool" >/dev/null || fail "required tool unavailable: $tool"
done
umask 077
mkdir -m 700 "$workdir/runtime" "$workdir/runtime/secrets" "$workdir/state" "$workdir/runtime/broker"
private="$workdir/runtime"
secrets="$private/secrets"
broker="$private/broker"
# Entire-file tokens have no trailing newline (charging credential checks read raw bytes).
for name in station-a station-b reader operator privileged; do
  if [[ "$name" == station-* ]]; then
    printf '%s' "S$(openssl rand -hex 8)" >"$secrets/$name"
  else
    printf 'uob1.demo.%s' "$(openssl rand -hex 32)" >"$secrets/$name"
  fi
done
read_a="$(<"$secrets/station-a")"
read_b="$(<"$secrets/station-b")"
reference_a="$(printf '%s' "$read_a" | sha256sum)"
reference_a="sha256:${reference_a%% *}"
reference_b="$(printf 'uob-charging-identity-v1\0Local\0%s' "${read_b^^}" | sha256sum)"
reference_b="sha256:${reference_b%% *}"

cat >"$private/client.toml" <<EOF
bridge_id = "$bridge"
environment = "demo"
target_instance_id = "main"
[[station]]
id = "station-a"
protocol = "ocpp16"
authorization_reference = "$reference_a"
[[station]]
id = "station-b"
protocol = "ocpp201"
authorization_reference = "$reference_b"
EOF
cat >"$private/simulator.toml" <<'EOF'
schema_version = 1
station_capacity = 2
[[stations]]
id = "station-a"
endpoint = "ws://127.0.0.1:19001/ocpp/station-a"
ocpp_version = "1.6"
request_timeout_ms = 10000
command_capacity = 8
trace_capacity = 64
step_capacity = 32
connectors = [1]
credentials_file = "/run/demo/secrets/station-a"
[[stations]]
id = "station-b"
endpoint = "ws://127.0.0.1:19001/ocpp/station-b"
ocpp_version = "2.0.1"
request_timeout_ms = 10000
command_capacity = 8
trace_capacity = 64
step_capacity = 32
evses = [{ id = 1, connectors = [1] }]
credentials_file = "/run/demo/secrets/station-b"
EOF
# A new run always starts from a fresh SQLite state; the first native OCPP 1.6 transaction is 1.
# Synthetic source times are slightly ahead of command admission and monotonic per transaction.
start_time="$(date -u -d '+2 minutes' +%Y-%m-%dT%H:%M:%SZ)"
meter_time="$(date -u -d '+3 minutes' +%Y-%m-%dT%H:%M:%SZ)"
stop_time="$(date -u -d '+4 minutes' +%Y-%m-%dT%H:%M:%SZ)"
status_time="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
while IFS= read -r line || [[ -n "$line" ]]; do
  line="${line//__START_A__/$read_a}"
  line="${line//__START_B__/$read_b}"
  line="${line//__START_TIME__/$start_time}"
  line="${line//__METER_TIME__/$meter_time}"
  line="${line//__STOP_TIME__/$stop_time}"
  printf '%s\n' "${line//__STATUS_TIME__/$status_time}"
done <"$root/packaging/compose/scenarios/charging.toml" >"$private/scenario.toml"
unset read_a read_b
cat >"$private/bridge.toml" <<EOF
[bridge]
id = "$bridge"
environment = "demo"
target_id = "main"
[management]
listen_addr = "127.0.0.1:18080"
[charging]
enabled = true
listen_addr = "127.0.0.1:19001"
state_directory = "/var/lib/uob/demo"
read_grant_file = "/run/demo/secrets/reader"
control_grant_file = "/run/demo/secrets/operator"
privileged_grant_file = "/run/demo/secrets/privileged"
[[charging.stations]]
id = "station-a"
protocol = "ocpp16j"
credential_file = "/run/demo/secrets/station-a"
start_token_file = "/run/demo/secrets/start-a"
allow_stop = true
[[charging.stations.resources]]
connector_id = "connector-1"
native_connector_id = 1
[[charging.stations]]
id = "station-b"
protocol = "ocpp201"
credential_file = "/run/demo/secrets/station-b"
start_token_file = "/run/demo/secrets/start-b"
allow_stop = true
[[charging.stations.resources]]
evse_id = "evse-1"
native_evse_id = 1
[[charging.stations.resources]]
evse_id = "evse-1"
connector_id = "connector-1"
native_evse_id = 1
native_connector_id = 1
[[targets]]
id = "main"
enabled = true
EOF
# Separate regular files: the daemon rejects repeated credential inode references.
cp "$secrets/station-a" "$secrets/start-a"
cp "$secrets/station-b" "$secrets/start-b"
if [[ "$mode" == ems-http ]]; then
  cat >>"$private/bridge.toml" <<'EOF'
kind = "ems-scada.http"
[targets.settings]
listen_addr = "127.0.0.1:19080"
credentials_file = "/run/demo/secrets/http-target.toml"
EOF
  cat >"$secrets/http-target.toml" <<EOF
[[principals]]
id = "reader"
token = "$(<"$secrets/reader")"
permissions = ["read"]
bridges = ["$bridge"]
[[principals]]
id = "operator"
token = "$(<"$secrets/operator")"
permissions = ["read", "control"]
bridges = ["$bridge"]
EOF
else
  profile=standard
  [[ "$mode" == ems-mqtt ]] && profile=ems-scada
  cat >>"$private/bridge.toml" <<EOF
kind = "mqtt"
[targets.settings]
broker_url = "mqtts://localhost:8883"
credentials_file = "/run/demo/secrets/mqtt-bridge.toml"
profile = "$profile"
EOF
  image='eclipse-mosquitto:2.0.22@sha256:212f89e1eaeb2c322d6441b64396e3346026674db8fa9c27beac293405c32b3c'
  openssl req -new -x509 -sha256 -newkey rsa:2048 -noenc -days 1 \
    -subj '/CN=UOB Compose demo CA' -addext 'basicConstraints=critical,CA:TRUE' \
    -addext 'keyUsage=critical,keyCertSign,cRLSign' \
    -keyout "$broker/ca.key" -out "$broker/ca.crt" >/dev/null 2>&1 || fail 'CA generation failed'
  openssl req -new -sha256 -newkey rsa:2048 -noenc -subj '/CN=localhost' \
    -keyout "$broker/server.key" -out "$broker/server.csr" >/dev/null 2>&1 || fail 'server key generation failed'
  printf '%s\n' 'subjectAltName=DNS:localhost,IP:127.0.0.1' 'extendedKeyUsage=serverAuth' \
    'keyUsage=digitalSignature,keyEncipherment' >"$broker/server.ext"
  openssl x509 -req -sha256 -days 1 -in "$broker/server.csr" \
    -CA "$broker/ca.crt" -CAkey "$broker/ca.key" \
    -set_serial "0x$(openssl rand -hex 16)" -extfile "$broker/server.ext" \
    -out "$broker/server.crt" >/dev/null 2>&1 || fail 'server certificate signing failed'
  rm -- "$broker/ca.key" "$broker/server.csr" "$broker/server.ext"
  bridge_password="$(openssl rand -hex 32)"
  printf 'username = "uob-bridge"\npassword = "%s"\nca_certificate_file = "/run/demo/broker/ca.crt"\n' \
    "$bridge_password" >"$secrets/mqtt-bridge.toml"
  cat >"$broker/mosquitto.conf" <<'EOF'
listener 8883 0.0.0.0
protocol mqtt
allow_anonymous false
password_file /run/demo/broker/passwords
acl_file /run/demo/broker/acl
cafile /run/demo/broker/ca.crt
certfile /run/demo/broker/server.crt
keyfile /run/demo/broker/server.key
persistence false
log_dest stdout
log_type error
EOF
  {
    printf 'user uob-bridge\ntopic read uob/v1/demo/%s/commands/+/+\n' "$bridge"
    for suffix in availability 'state/+' 'points/+/+' 'values/+/+' 'events/+/+' 'results/+/+' 'traces/+/+'; do
      printf 'topic write uob/v1/demo/%s/%s\n' "$bridge" "$suffix"
    done
    for user in ems-reader ems-operator; do
      printf 'user %s\n' "$user"
      if [[ "$user" == ems-operator ]]; then
        printf 'topic write uob/v1/demo/%s/commands/+/+\n' "$bridge"
      fi
      for suffix in availability 'state/+' 'points/+/+' 'values/+/+' 'events/+/+' 'results/+/+' 'traces/+/+'; do
        printf 'topic read uob/v1/demo/%s/%s\n' "$bridge" "$suffix"
      done
    done
  } >"$broker/acl"
  password_account() {
    local create="$1" name="$2" password="$3"
    local args=()
    [[ "$create" == true ]] && args+=(-c)
    printf '%s\n%s\n' "$password" "$password" |
      docker run --rm --interactive --network none --user "$(id -u):$(id -g)" \
        --cap-drop ALL --security-opt no-new-privileges \
        --mount "type=bind,src=$broker,dst=/fixture" --entrypoint mosquitto_passwd \
        "$image" "${args[@]}" /fixture/passwords "$name" >/dev/null || fail 'MQTT account provisioning failed'
  }
  password_account true uob-bridge "$bridge_password"
  password_account false ems-reader "$(<"$secrets/reader")"
  password_account false ems-operator "$(<"$secrets/operator")"
  unset bridge_password
fi
# The final ownership transition happens only after all files and ACLs are written.
chmod 0700 "$private" "$secrets" "$broker" "$workdir/state"
find "$private" -type f -exec chmod 0600 {} +
docker run --rm --network none --user 0 --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE \
  --security-opt no-new-privileges --mount "type=bind,src=$workdir,dst=/fixture" \
  --entrypoint chown debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 \
  -R 10001:10001 /fixture/runtime /fixture/state >/dev/null || fail 'UID 10001 ownership setup failed'
