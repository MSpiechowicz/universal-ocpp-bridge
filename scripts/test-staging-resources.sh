#!/usr/bin/env bash
# Root on a disposable systemd/cgroup-v2 CI host only. Never a charging device.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ "${1:-}" != --disposable ]]; then
  echo 'requires explicit --disposable on an isolated systemd CI host' >&2
  exit 2
fi
if [[ "$EUID" != 0 ]]; then
  exec sudo -- "$0" --disposable
fi
[[ -f /sys/fs/cgroup/cgroup.controllers ]]
# Independent unique units; never install, start, stop, or change real UOB units.
prefix="uobtest${BASHPID}"
slice="$prefix.slice"
guard="$prefix-governor.service"
peer="$prefix-peer.service"
production="$prefix-production.service"
root="$(mktemp -d /run/uob-resource-test.XXXXXX)"
cleanup() {
  systemctl stop "$slice" "$guard" "$production" || true
  rm -f "/run/systemd/system/$slice" "/run/systemd/system/$guard"
  systemctl daemon-reload
  rm -rf "$root"
}
trap cleanup EXIT
python3 - "$root" "$slice" <<'PY'
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
slice_name = sys.argv[2]
source = Path('packaging/resources/staging_governor.py').read_text()
source = source.replace("SLICE = 'uob-staging.slice'", f'SLICE = {slice_name!r}')
source = source.replace('/sys/fs/cgroup/uob.slice/uob-staging.slice',
                        '/sys/fs/cgroup/' + slice_name)
(root / 'governor.py').write_text(source)
policy = json.loads(Path('packaging/resources/staging-policy.json').read_text())
policy.update(memory_high_mib=64, memory_max_mib=96, persistence_seconds=2,
              cohosted=True)
(root / 'policy.json').write_text(json.dumps(policy))
PY
sed -e "s/uob-staging-governor.service/$guard/g" \
    -e 's/MemoryHigh=384M/MemoryHigh=64M/' -e 's/MemoryMax=512M/MemoryMax=96M/' \
    packaging/systemd/uob-staging.slice >"/run/systemd/system/$slice"
sed -e "s/uob-staging.slice/$slice/g" \
    -e "s|/usr/local/libexec/staging_governor.py|$root/governor.py|" \
    -e "s|/etc/uob-staging/staging-policy.json|$root/policy.json|" \
    packaging/systemd/uob-staging-governor.service >"/run/systemd/system/$guard"
systemctl daemon-reload
systemd-run --unit="$production" --property=MemoryMax=256M \
  /usr/bin/python3 "$PWD/scripts/staging_health_fixture.py" "$root"
for _ in {1..20}; do
  [[ -e "$root/port" ]] && break
  sleep 0.1
done
python3 - "$root" <<'PYCODE'
import json, sys
from pathlib import Path
root=Path(sys.argv[1]); p=root/'policy.json'; value=json.loads(p.read_text())
value['production_port']=int((root/'port').read_text())
p.write_text(json.dumps(value))
PYCODE
production_pid="$(systemctl show "$production" --property=MainPID --value)"
start_peer() {
  systemd-run --unit="$peer" --slice="$slice" \
    --property="Requires=$guard" --property="After=$guard" \
    --property="BindsTo=$guard" "$@"
}
wait_stopped() {
  for _ in {1..20}; do
    if ! systemctl is-active --quiet "$slice"; then
      [[ "$(systemctl show "$production" --property=MainPID --value)" == "$production_pid" ]]
      systemctl is-active --quiet "$production"
      return
    fi
    sleep 1
  done
  journalctl -u "$guard" --no-pager
  echo 'staging did not stop within its deadline' >&2
  exit 1
}
# A real load storm remains CPU-capped and the host's independent process lives.
start_peer /usr/bin/python3 -c 'import time; end=time.monotonic()+5
while time.monotonic()<end: pass
time.sleep(120)'
sleep 4
awk '$1 == "nr_throttled" { if ($2 > 0) ok=1 } END { exit !ok }' \
  "/sys/fs/cgroup/$slice/cpu.stat"
[[ "$(cat "/sys/fs/cgroup/$slice/memory.max")" == 100663296 ]]
# A real HTTP health alarm persists for the configured duration and sheds only staging.
touch "$root/alarm"
wait_stopped
journalctl -u "$guard" --no-pager | grep '"reason": "production_alarm"'
rm "$root/alarm"
peer="$prefix-second-peer.service"
systemctl reset-failed "$guard" || true
start_peer /usr/bin/sleep 120
# A different descendant triggers a real OOM: both peers must stop, while
# the production sentinel retains exactly the same PID.
systemd-run --unit="$prefix-oom" --slice="$slice" \
  --property="Requires=$guard" --property="After=$guard" \
  /usr/bin/python3 -c 'x=bytearray(160*1024*1024); import time; time.sleep(60)'
wait_stopped
journalctl -u "$guard" --no-pager | grep 'staging_oom'
if systemctl is-active --quiet "$peer"; then
  echo 'peer survived staging shedding' >&2
  exit 1
fi
# Missing/enforcement-mismatched controls refuse readiness and peer execution.
python3 - "$root/policy.json" <<'PY'
import json, sys
from pathlib import Path
p=Path(sys.argv[1]); value=json.loads(p.read_text()); value['memory_max_mib']=97
p.write_text(json.dumps(value))
PY
systemctl reset-failed "$guard" || true
peer="$prefix-rejected-peer.service"
if start_peer /usr/bin/touch "$root/incorrectly-admitted"; then
  echo 'mismatched controls incorrectly admitted peer' >&2
  exit 1
fi
[[ ! -e "$root/incorrectly-admitted" ]]
[[ "$(systemctl show "$production" --property=MainPID --value)" == "$production_pid" ]]
echo 'real cgroup CPU limiting, OOM shedding, and fail-closed admission verified'
