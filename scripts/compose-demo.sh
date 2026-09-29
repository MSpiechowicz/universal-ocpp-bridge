#!/usr/bin/env bash
# Usage: compose-demo.sh run|headless|browser|up MODE; down|cleanup RUN_DIRECTORY
set -euo pipefail
fail() { printf 'Compose demo: %s\n' "$*" >&2; exit 1; }
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose_file="$root/packaging/compose/compose.yaml"
[[ $# == 2 ]] || fail 'usage: compose-demo.sh run|headless|browser|up mqtt|ems-http|ems-mqtt; down|cleanup RUN_DIRECTORY'
action="$1" argument="$2"
for tool in docker openssl jq timeout; do
  command -v "$tool" >/dev/null || fail "required tool unavailable: $tool"
done
docker info >/dev/null 2>&1 || fail 'Docker daemon unavailable or access denied'
docker compose version >/dev/null 2>&1 || fail 'Docker Compose v2 is required'
umask 077

load_run() {
  local path="$1"
  [[ "$path" == "$root"/target/compose-demo.* && ! -L "$path" && -d "$path" ]] || fail 'run directory must be a provisioned private target/compose-demo.* path'
  [[ -f "$path/instance" && ! -L "$path/instance" ]] || fail 'run metadata unavailable'
  local owner mode project bridge
  IFS=' ' read -r owner mode project bridge <"$path/instance"
  [[ "$owner" == compose-demo-94 && "$mode" =~ ^(mqtt|ems-http|ems-mqtt)$ && "$project" =~ ^uob94-[0-9a-f]{16}$ && "$bridge" =~ ^site-[0-9a-f]{16}$ ]] || fail 'run metadata invalid'
  [[ "${path##*/}" == "compose-demo.${project#uob94-}" ]] || fail 'run metadata does not match directory'
  workdir="$path" target_mode="$mode" demo_project="$project"
  export DEMO_ROOT="$workdir" DEMO_PROJECT="$demo_project"
  compose=(docker compose -p "$demo_project" -f "$compose_file" --profile "$target_mode")
  if [[ -f "$workdir/browser.yaml" ]]; then
    compose+=(--profile browser -f "$workdir/browser.yaml")
  fi
}

stop_run() {
  "${compose[@]}" down --remove-orphans
}
remove_run() {
  local paths=()
  [[ -d "$workdir/runtime" ]] && paths+=(/fixture/runtime)
  [[ -d "$workdir/state" ]] && paths+=(/fixture/state)
  if [[ (-d "$workdir/runtime" && ! -O "$workdir/runtime") ||
        (-d "$workdir/state" && ! -O "$workdir/state") ]]; then
    # CHOWN changes the owner; DAC_OVERRIDE traverses the host's 0700 run root.
    docker run --rm --network none --user 0 --cap-drop ALL \
      --cap-add CHOWN --cap-add DAC_OVERRIDE --security-opt no-new-privileges \
      --mount "type=bind,src=$workdir,dst=/fixture" --entrypoint chown \
      debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 \
      -R "$(id -u):$(id -g)" "${paths[@]}" >/dev/null || return 1
  fi
  rm -rf -- "$workdir"
}

case "$action" in
  down|cleanup)
    load_run "$argument"
    stop_run || fail 'project shutdown failed; run directory retained'
    if [[ "$action" == cleanup ]]; then
      remove_run || fail 'private file cleanup failed; run directory retained'
    fi
    exit 0
    ;;
  run|headless|browser|up) ;;
  *) fail 'unknown action (run, headless, browser, up, down, cleanup)' ;;
esac
[[ "$argument" == mqtt || "$argument" == ems-http || "$argument" == ems-mqtt ]] || fail 'target mode must be mqtt, ems-http, or ems-mqtt'
mkdir -p "$root/target"
nonce="$(openssl rand -hex 8)"
workdir="$root/target/compose-demo.$nonce"
mkdir -m 700 "$workdir" || fail 'unable to create private run directory'
demo_project="uob94-$nonce"
bridge="site-$nonce"
export DEMO_ROOT="$workdir" DEMO_PROJECT="$demo_project"
target_mode="$argument"
compose=(docker compose -p "$demo_project" -f "$compose_file" --profile "$target_mode")
started=false
on_exit() {
  local code=$?
  trap - EXIT INT TERM
  if [[ "$started" == true ]] && ! "${compose[@]}" down --remove-orphans >/dev/null; then
    printf 'Compose demo: project shutdown failed; private run directory retained\n' >&2
    exit 1
  fi
  if ! remove_run; then
    printf 'Compose demo: private file cleanup failed; run directory retained\n' >&2
    code=1
  fi
  exit "$code"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
printf 'compose-demo-94 %s %s %s\n' "$argument" "$demo_project" "$bridge" >"$workdir/instance"
if [[ "$action" == browser ]]; then
  cat >"$workdir/browser.yaml" <<'EOF'
services:
  daemon:
    command: [serve, --config, /run/demo/bridge.toml]
    ports:
      - "127.0.0.1::18000"
EOF
  compose+=(--profile browser -f "$workdir/browser.yaml")
fi
printf 'Compose project: %s\nRun directory: %s\n' "$demo_project" "$workdir"
"${compose[@]}" config --quiet || fail 'Compose configuration invalid'
case "$target_mode" in
  mqtt) client=client-mqtt ;;
  ems-http) client=client-http ;;
  ems-mqtt) client=client-ems-mqtt ;;
esac
# All executable images come from the local Dockerfile, never a registry binary image.
"${compose[@]}" build daemon simulator "$client" || fail 'local executable image build failed'
"$root/scripts/compose-demo-provision.sh" "$workdir" "$bridge" "$target_mode" || fail 'private provisioning failed'
services=(daemon)
[[ "$target_mode" == ems-http ]] || services+=(broker)
[[ "$action" == browser ]] && services+=(gateway)
started=true
"${compose[@]}" up -d --no-build "${services[@]}" || fail 'daemon/broker startup failed'
if [[ "$action" == browser ]]; then
  gateway="$("${compose[@]}" port daemon 18000)" || fail 'gateway port lookup failed'
  [[ "$gateway" =~ ^127\.0\.0\.1:([0-9]+)$ ]] || fail 'gateway did not bind host loopback'
  printf 'Browser gateway: http://127.0.0.1:%s/\n' "${BASH_REMATCH[1]}"
fi
# The daemon owns the charging socket; no host port is opened to reach it.
for ((attempt = 0; attempt < 30; attempt++)); do
  daemon_id="$("${compose[@]}" ps -q daemon)"
  if [[ -n "$daemon_id" && "$(docker inspect --format '{{.State.Running}}' "$daemon_id")" == true ]] &&
     "${compose[@]}" exec -T daemon /usr/local/bin/uob config check --config /run/demo/bridge.toml >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
[[ "$attempt" -lt 30 ]] || fail 'daemon readiness timed out'
if [[ "$action" == up ]]; then
  printf 'Project started headless (no simulator or client); run %s MODE to verify a NEW private project.\n' "$0"
  printf 'Stop: %s down %s\nRemove private files: %s cleanup %s\n' "$0" "$workdir" "$0" "$workdir"
  trap - EXIT INT TERM
  exit 0
fi
"${compose[@]}" up -d --no-build --no-recreate simulator || fail 'simulator launch failed'
sim_id="$("${compose[@]}" ps -q simulator)"
[[ -n "$sim_id" ]] || fail 'simulator container unavailable'
# uob-sim writes its report at run completion. The peer waits for live station
# readiness before issuing commands, so the two independent containers overlap.
# Client reads only mounted files; tokens never appear in command arguments or logs.
timeout 180 "${compose[@]}" run --no-deps --rm "$client" >"$workdir/client.log" 2>&1 || {
  cat "$workdir/client.log" >&2
  docker logs "$sim_id" 2>/dev/null |
    jq -Rc 'fromjson? | select(.event == "step_failed" or .event == "run_failed") | {event,status,step_id,action,failure_code,diagnostics}' >&2 || true
  fail 'target peer verification failed'
}
cat "$workdir/client.log"
sim_exit="$(timeout 120 docker wait "$sim_id")" || fail 'simulator completion timed out'
docker logs "$sim_id" 2>/dev/null | jq -c 'select(.event == "run_passed" or .event == "run_failed") | {event, status, failure_code, diagnostics}' >"$workdir/simulator-summary.jsonl" || fail 'simulator report unavailable'
cat "$workdir/simulator-summary.jsonl"
[[ "$sim_exit" == 0 ]] || fail 'simulator scenario failed'
jq -e 'select(.event == "run_passed")' "$workdir/simulator-summary.jsonl" >/dev/null || fail 'simulator did not report success'
if [[ "$action" == browser ]]; then
  printf 'Browser gateway remains available until interrupted; project shutdown follows.\n'
  docker wait "$daemon_id" >/dev/null || fail 'daemon exited unexpectedly'
fi
