#!/usr/bin/env bash
# Explicit opt-in only: external Python protocol stack, never default build/demo/CI.
set -euo pipefail
if [[ $# != 2 ]]; then
  echo "usage: $0 /absolute/path/to/packaged/uob-sim /new/evidence-directory" >&2
  exit 2
fi
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
simulator="$(realpath "$1")"
evidence="$(realpath -m "$2")"
[[ -x "$simulator" && ! -e "$evidence" ]]
context="$(mktemp -d /tmp/uob-external-csms.XXXXXXXX)"
image="uob-external-csms:$(basename "$context" | tr '[:upper:]' '[:lower:]')"
container=""
cleanup() {
  if [[ -n "$container" ]]; then docker rm -f "$container" >/dev/null; fi
  docker image rm "$image" >/dev/null 2>&1 || true
  rm -rf "$context"
}
trap cleanup EXIT
cp "$simulator" "$context/uob-sim"
for file in Dockerfile requirements.txt peer.py run.py test_evidence.py; do
  cp "$repository_root/tests/external-csms/$file" "$context/$file"
done
cp -R "$repository_root/tests/external-csms/scenarios" "$context/scenarios"
docker build --tag "$image" "$context"
mkdir -p "$evidence"
docker image inspect "$image" --format '{{.Id}}' > "$evidence/image-id.txt"
# No repository/host mounts, external network, service, database, or Rust source.
container="$(docker create --network none --read-only \
  --tmpfs /tmp:rw,nosuid,size=32m --memory=256m --cpus=1 --pids-limit=64 \
  --cap-drop=ALL --security-opt=no-new-privileges \
  --env UOB_SIM=/usr/local/bin/uob-sim --entrypoint python "$image" test_evidence.py)"
timeout --kill-after=5s 120s docker start --attach "$container"
docker rm -f "$container" >/dev/null
container=""
for mode in smoke mismatch; do
  arguments=()
  if [[ "$mode" == mismatch ]]; then arguments+=(--inject-mismatch); fi
  container="$(docker create --network none --read-only \
    --tmpfs /tmp:rw,nosuid,size=32m --memory=256m --cpus=1 --pids-limit=64 \
    --cap-drop=ALL --security-opt=no-new-privileges \
    "$image" --output /tmp/evidence "${arguments[@]}")"
  set +e
  timeout --kill-after=5s 100s docker start --attach "$container" > "$evidence/$mode.log" 2>&1
  status=$?
  set -e
  # The JSON report on stdout survives teardown of the private tmpfs.
  python3 - "$evidence/$mode.log" "$evidence/$mode.json" <<'PY'
import json,sys
report=json.loads(open(sys.argv[1]).read())
with open(sys.argv[2], 'w') as out:
    json.dump(report,out,indent=2)
    out.write('\n')
PY
  docker rm -f "$container" >/dev/null
  container=""
  if [[ "$mode" == smoke ]]; then
    [[ "$status" == 0 ]]
  else
    [[ "$status" == 1 ]]
  fi
done
python3 - "$evidence" <<'PY'
import json, pathlib, sys
root=pathlib.Path(sys.argv[1])
smoke=json.loads((root/'smoke.json').read_text())
mismatch=json.loads((root/'mismatch.json').read_text())
assert smoke['status']=='passed'
assert {c['protocol'] for c in smoke['cases']}=={'1.6','2.0.1'}
assert all(c['status']=='passed' for c in smoke['cases'])
assert mismatch['status']=='failed'
assert len(mismatch['cases'])==2
assert all(c['status']=='failed' and c['simulator_exit_code']==3
           and c['failure_code']=='unexpected_protocol_response'
           and c['observed']==['BootNotification'] for c in mismatch['cases'])
print('External smoke passed; both deliberate mismatches failed as required.')
PY
