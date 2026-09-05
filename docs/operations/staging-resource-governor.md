# Staging resource admission and shedding

The root-owned Python 3 standard-library helper `packaging/resources/staging_governor.py`
supervises the **complete staging slice** from `system.slice`. It never starts, stops, kills,
or restarts production. Production boots independently and keeps its existing restart policy.
The helper is an operations process, separate from both the charging daemon and the future
artifact release supervisor. No compilers or Python packages are needed on the device.

The shipped cgroup v2 limits apply cumulatively to the candidate, simulator, broker, database
and test providers:

| Control | Staging slice | Production |
|---|---|---|
| MemoryHigh | 384 MiB | Existing daemon budget |
| MemoryMax | 512 MiB | 256 MiB on `uob.service` |
| Swap | Disabled for staging | Host policy |
| CPUQuota | 50% of one core | No new quota |
| CPUWeight / IOWeight | 25 / 25 | 100 / 100 |
| OOM | Governor stops all staging descendants | Separate cgroup |

The production steady-state acceptance target remains 128 MiB RSS. These are initial controls,
not measured Raspberry Pi capacity claims. Builds and 100-charger overload qualification belong
on a separate test host, never a charging Pi.

## Admission and runtime behavior

systemd realizes the staging slice before starting the governor. Every staging service must
order after the governor's `Type=notify` readiness. The helper reads the actual kernel limits,
including CPU quota, IO weight, swap and memory events, before notifying readiness. Missing cgroup
v2 controllers, unbounded values, ignored directives, mismatches with policy, unavailable
`MemAvailable`, or less than 512 MiB available host memory refuse admission. There is no
unbounded fallback. In co-hosted mode the existing production loopback `/health` route must also
be healthy before staging starts.

Once per second the helper checks enforced limits, host memory, hierarchical staging memory
counters and production health. A new staging `oom` or `oom_kill` event immediately queues a
stop of the entire slice. Sustained MemoryHigh throttling or resident usage above MemoryHigh, host available memory below 256 MiB,
or production alarms stop staging after 30 continuous seconds. Recovery resets each independent
condition's clock; wall-clock corrections cannot change the monotonic duration. The next sample
and bounded probe add detection latency. Existing historical OOM counters establish a baseline
when an operator explicitly restarts staging.

Production alarms include core/storage unavailability, refused new sessions, local response
p95 over 100 ms, RSS over 128 MiB, or a slow, failed, malformed or oversized health response.
The HTTP probe uses only loopback, bounded reads, a one-second socket timeout and no credentials,
redirects or control methods. Target/export component degradation alone is not a production alarm.
The governor's systemd watchdog stops it if monitoring stalls for ten seconds; slice `BindsTo`
then sheds staging. A crashed/killed governor likewise stops the slice. Staging stays stopped
until an explicit operator start; the governor has no automatic restart policy.

JSON journal records contain the full effective policy on each start, admission, pressure onset
and stop reason. Inspect `journalctl -u uob-staging-governor.service` and
`systemctl status uob-staging-governor.service uob-staging.slice`. If admission fails, restore
host headroom or correct the named kernel/policy mismatch, then explicitly start staging again.
Do not weaken controls just to pass admission. Normal governor termination is additionally
observable through systemd's unit result, including watchdog and signal failures.

## Installation and peer wiring

Follow [filesystem isolation](environment-filesystem-isolation.md) and
[network isolation](environment-network-isolation.md) first. Install these reviewed files as an
administrator; do not overwrite an existing policy without review:

```sh
install -m 0644 packaging/resources/staging_governor.py /usr/local/libexec/staging_governor.py
install -m 0640 -o root -g root packaging/resources/staging-policy.json /etc/uob-staging/staging-policy.json
install -m 0644 packaging/systemd/uob-staging-governor.service /etc/systemd/system/
install -m 0644 packaging/systemd/uob-staging.slice packaging/systemd/uob-production.slice /etc/systemd/system/
install -m 0644 packaging/systemd/uob.service packaging/systemd/uob-staging.service /etc/systemd/system/
systemctl daemon-reload
# Apply the production daemon cap during its next planned restart.
# Start staging only after the production cap and staging limits are in effect.
systemctl start uob-staging.service
```

Install Python 3 from the OS before starting staging. Keep the helper, policy and units root-owned
and non-writable by either environment's service user. Threshold changes are explicit reviewed
configuration: edit `staging-policy.json`; changes to memory/CPU/IO limits must also be made in a
matching slice drop-in. Restart staging to apply and journal the new policy. Unknown fields,
invalid values, CPU quota above 50%, or staging weights at/above 100 fail closed. On a separate
Linux staging host set `cohosted` to `false`; kernel checks and memory shedding still apply.
For a non-default production loopback port, set `production_port` explicitly. A TLS-only management
listener needs an appropriate local health endpoint before co-hosting can be admitted.

Every simulator, test broker, test database and provider unit must include these dependencies,
in addition to the existing isolated network/user/filesystem settings:

```ini
[Unit]
Requires=uob-staging-governor.service
After=uob-staging-governor.service
BindsTo=uob-staging-governor.service

[Service]
Slice=uob-staging.slice
```

Do not start peers as unmanaged shell processes or independent containers outside the slice.
Do not start a peer merely after `uob-staging.slice`: its readiness is not governor admission.
The shipped candidate unit includes the required ordering. Slices alone cannot enforce correct
admission ordering for arbitrary administrator-created units. Production must never join the
staging slice or depend on the staging governor.

## Verification

`python3 -B scripts/test_staging_governor.py` exercises timing boundaries, condition recovery,
aggregate OOM counters, malformed/missing kernel controls, low-memory and unhealthy-production
admission, policy validation, bounded health parsing, and staging-only stop requests. The full
workspace verifier runs it.

`./scripts/test-staging-resources.sh --disposable` requires root, systemd and cgroup v2 on a
**disposable Linux test host**. CI runs it after workspace verification. It creates unique
runtime-only test units, uses the shipped slice/governor with smaller test limits, verifies
actual kernel CPU throttling and persistent production HTTP alarms, causes a real descendant OOM and observes whole-slice
shutdown, verifies a separate production sentinel keeps its PID, and rejects peer admission
when kernel controls disagree with policy. It removes only its own temporary units/files.
It never manipulates deployed UOB units or claims measured Pi response/memory performance.
