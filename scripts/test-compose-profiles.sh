#!/usr/bin/env bash
# Opt-in issue-94 acceptance via the public launcher; never selects another Docker project.
set -euo pipefail

fail() { printf 'Compose profile acceptance: %s\n' "$*" >&2; exit 1; }
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
launcher="$root/scripts/compose-demo.sh"
for tool in docker jq timeout; do
  command -v "$tool" >/dev/null || fail "required tool unavailable: $tool"
done
docker info >/dev/null 2>&1 || fail 'Docker daemon unavailable or access denied'
docker compose version >/dev/null 2>&1 || fail 'Docker Compose v2 is required'
seconds="${DEMO_PROFILE_TIMEOUT_SECONDS:-1800}"
[[ "$seconds" =~ ^[1-9][0-9]*$ ]] || fail 'DEMO_PROFILE_TIMEOUT_SECONDS must be a positive integer'
umask 077
mkdir -p "$root/target"
logs="$(mktemp -d "$root/target/compose-acceptance.XXXXXXXX")" || fail 'unable to create private acceptance log directory'
project='' run_dir=''

# Parse only the two identifiers printed by the launcher. Its own cleanup validates
# run metadata before touching resources; never fall back to broad Docker cleanup.
parse_run() {
  local line suffix
  project='' run_dir=''
  while IFS= read -r line; do
    case "$line" in
      'Compose project: '*) project="${line#Compose project: }" ;;
      'Run directory: '*) run_dir="${line#Run directory: }" ;;
    esac
  done <"$1"
  [[ "$project" =~ ^uob94-[0-9a-f]{16}$ ]] || return 1
  suffix="${project#uob94-}"
  [[ "$run_dir" == "$root/target/compose-demo.$suffix" ]]
}

# Called after timeout has reaped its process group, or after a completed run.
# If the launcher failed before provisioning, its metadata may not be usable:
# report that failure rather than touching another project.
check_cleanup() {
  local containers networks
  if [[ -d "$run_dir" ]]; then
    "$launcher" cleanup "$run_dir" >&2 || fail "owned run directory retained: $run_dir"
    fail "launcher left private state after $1 (removed it via explicit cleanup)"
  fi
  containers="$(docker ps -aq --filter "label=com.docker.compose.project=$project")" || fail 'unable to inspect project containers'
  networks="$(docker network ls -q --filter "label=com.docker.compose.project=$project")" || fail 'unable to inspect project networks'
  [[ -z "$containers" && -z "$networks" ]] || fail "owned project not removed after $1: $project"
}

on_exit() {
  local code=$?
  trap - EXIT INT TERM
  if ((code != 0)) && [[ -n "$run_dir" && -d "$run_dir" ]]; then
    "$launcher" cleanup "$run_dir" >&2 || printf 'Private state retained; retry: %s cleanup %s\n' "$launcher" "$run_dir" >&2
  fi
  rm -rf -- "$logs"
  exit "$code"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# A rejected mode must not create a project and must not be mistaken for a pass.
if "$launcher" headless unsupported >"$logs/invalid.log" 2>&1; then
  fail 'unsupported mode unexpectedly succeeded'
fi
if ! grep -q 'target mode must be mqtt, ems-http, or ems-mqtt' "$logs/invalid.log"; then
  fail 'unsupported mode did not fail at mode validation'
fi

for mode in mqtt ems-http ems-mqtt; do
  log="$logs/$mode.log"
  printf 'Checking %s with real charging and target peers\n' "$mode"
  status=0
  timeout --signal=TERM --kill-after=45 "$seconds" "$launcher" headless "$mode" >"$log" 2>&1 || status=$?
  if ! parse_run "$log"; then
    cat "$log" >&2
    fail "launcher did not supply owned project/run identifiers for $mode (exit $status)"
  fi
  if ((status != 0)); then
    cat "$log" >&2
    check_cleanup "$mode failure"
    fail "$mode exited with status $status"
  fi

  # These lines come from separate live protocol exchanges, not configuration text.
  for protocol in ocpp16 ocpp201; do
    if ! grep -q "^charging verified: protocol=$protocol " "$log"; then
      cat "$log" >&2
      fail "$mode did not verify $protocol charging with its target peer"
    fi
  done
  if ! jq -R -e 'fromjson? | select(.event == "run_passed" and .status == "passed")' "$log" >/dev/null 2>&1; then
    cat "$log" >&2
    fail "$mode simulator did not report run_passed"
  fi
  check_cleanup "$mode success"
  project='' run_dir=''
  printf 'Passed %s\n' "$mode"
done
printf 'All three Compose target profiles passed\n'
