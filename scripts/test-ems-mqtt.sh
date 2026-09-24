#!/usr/bin/env bash
# Opt-in issue-88 acceptance: private TLS Mosquitto, per-run users, no fixed host port.
set -euo pipefail

fail() { printf 'EMS MQTT acceptance: %s\n' "$*" >&2; exit 1; }
for tool in docker openssl cargo timeout; do
  command -v "$tool" >/dev/null 2>&1 || fail "required tool unavailable: $tool"
done
docker info >/dev/null 2>&1 || fail 'Docker daemon unavailable or access denied'
image='eclipse-mosquitto:2.0.22'
docker image inspect "$image" >/dev/null 2>&1 || fail "Mosquitto image unavailable locally: $image (pull it explicitly before retrying)"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$root/tests/ems-mqtt-broker"
[[ -f "$fixture/acl" && -f "$fixture/mosquitto.conf" ]] || fail 'Mosquitto fixture files unavailable'
umask 077
# Docker Desktop only shares workspace paths, not arbitrary host /tmp bind mounts.
mkdir -p "$root/target"
workdir="$(mktemp -d "$root/target/uob-ems-mqtt.XXXXXXXX")" || fail 'unable to create private temporary directory'
container_id=''
cleanup() {
  if [[ -n "$container_id" ]]; then
    docker rm --force "$container_id" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$workdir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cp "$fixture/acl" "$fixture/mosquitto.conf" "$workdir/"
openssl req -new -x509 -sha256 -newkey rsa:2048 -noenc -days 1 \
  -subj '/CN=UOB MQTT fixture CA' \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -keyout "$workdir/ca.key" -out "$workdir/ca.crt" >/dev/null 2>&1 \
  || fail 'unable to generate ephemeral CA'
openssl req -new -sha256 -newkey rsa:2048 -noenc \
  -subj '/CN=localhost' \
  -keyout "$workdir/server.key" -out "$workdir/server.csr" >/dev/null 2>&1 \
  || fail 'unable to generate ephemeral server key and CSR'
printf '%s\n' \
  'subjectAltName=DNS:localhost,IP:127.0.0.1' \
  'extendedKeyUsage=serverAuth' \
  'keyUsage=digitalSignature,keyEncipherment' >"$workdir/server.ext"
openssl x509 -req -sha256 -days 1 -in "$workdir/server.csr" \
  -CA "$workdir/ca.crt" -CAkey "$workdir/ca.key" -set_serial "0x$(openssl rand -hex 16)" \
  -extfile "$workdir/server.ext" -out "$workdir/server.crt" >/dev/null 2>&1 \
  || fail 'unable to sign ephemeral server certificate'

bridge_password="$(openssl rand -hex 32)"
reader_password="$(openssl rand -hex 32)"
operator_password="$(openssl rand -hex 32)"
printf '%s\n' "$reader_password" >"$workdir/reader.password"
printf '%s\n' "$operator_password" >"$workdir/operator.password"
printf 'username = "uob-bridge"\npassword = "%s"\nca_certificate_file = "%s/ca.crt"\n' \
  "$bridge_password" "$workdir" >"$workdir/bridge-credentials.toml"

# Passwords enter only through stdin: never Docker arguments, environment or logs.
password_account() {
  local create="$1" user="$2" password="$3"
  local args=()
  if [[ "$create" == true ]]; then args+=(-c); fi
  printf '%s\n%s\n' "$password" "$password" |
    docker run --rm --interactive --network none \
      --user "$(id -u):$(id -g)" --cap-drop ALL --security-opt no-new-privileges \
      --mount "type=bind,src=$workdir,dst=/fixture" \
      --entrypoint mosquitto_passwd "$image" "${args[@]}" /fixture/passwords "$user" \
      >/dev/null || fail "unable to provision $user MQTT account"
}
password_account true uob-bridge "$bridge_password"
password_account false ems-reader "$reader_password"
password_account false ems-operator "$operator_password"
unset bridge_password reader_password operator_password

# The config exposes only TLS/8883; Docker binds a random host port on loopback.
container_id="$(docker run --detach --rm \
  --name "uob-ems-mqtt-${workdir##*.}" \
  --user "$(id -u):$(id -g)" \
  --read-only --tmpfs /mosquitto/data:rw,nosuid,noexec,size=4m \
  --tmpfs /mosquitto/log:rw,nosuid,noexec,size=4m \
  --cap-drop ALL --security-opt no-new-privileges \
  --publish '127.0.0.1::8883' \
  --mount "type=bind,src=$workdir,dst=/fixture,readonly" \
  --entrypoint mosquitto "$image" -c /fixture/mosquitto.conf)" \
  || fail 'unable to start isolated TLS Mosquitto container'
port_mapping="$(docker port "$container_id" 8883/tcp)" || fail 'unable to discover loopback MQTT port'
[[ "$port_mapping" =~ ^127\.0\.0\.1:([0-9]+)$ ]] || fail "unexpected MQTT port mapping: $port_mapping"
port="${BASH_REMATCH[1]}"
ready=false
for ((attempt = 0; attempt < 30; attempt++)); do
  if timeout 2 openssl s_client -connect "127.0.0.1:$port" \
    -servername localhost -verify_hostname localhost \
    -CAfile "$workdir/ca.crt" -verify_return_error </dev/null >/dev/null 2>&1; then
    ready=true
    break
  fi
  if [[ "$(docker inspect --format '{{.State.Running}}' "$container_id" 2>/dev/null)" != true ]]; then
    docker logs "$container_id" >&2 || true
    fail 'TLS Mosquitto exited before readiness'
  fi
  sleep 1
done
[[ "$ready" == true ]] || fail 'TLS Mosquitto readiness timed out (30 attempts)'

export UOB_MQTT_BROKER_URL="mqtts://localhost:$port"
export UOB_MQTT_CA_FILE="$workdir/ca.crt"
export UOB_MQTT_TARGET_CREDENTIALS_FILE="$workdir/bridge-credentials.toml"
export UOB_MQTT_CLIENT_USER='ems-operator'
export UOB_MQTT_CLIENT_PASSWORD_FILE="$workdir/operator.password"
export UOB_MQTT_READER_USER='ems-reader'
export UOB_MQTT_READER_PASSWORD_FILE="$workdir/reader.password"
export UOB_MQTT_BROKER_CONTAINER="$container_id"

cd "$root"
cargo build --locked -p uob-mqtt-target-adapter --example probe_mqtt_contract
target_dir="${CARGO_TARGET_DIR:-$root/target}"
if [[ "$target_dir" != /* ]]; then target_dir="$root/$target_dir"; fi
export UOB_MQTT_EXAMPLE_PATH="$target_dir/debug/examples/probe_mqtt_contract"
[[ -x "$UOB_MQTT_EXAMPLE_PATH" ]] || fail "built MQTT probe executable unavailable: $UOB_MQTT_EXAMPLE_PATH"
cargo test --locked -p uob-mqtt-target-adapter --test issue88_integration -- \
  --ignored --exact issue88_integration --nocapture --color never | tee "$workdir/test-output.log"
[[ "$(<"$workdir/test-output.log")" == *'running 1 test'* ]] \
  || fail 'ignored issue88_integration did not run exactly once'
printf 'EMS MQTT TLS acceptance passed\n'
