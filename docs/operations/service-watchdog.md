# Service readiness, watchdog and termination evidence

The packaged production and staging services use `Type=notify`, `NotifyAccess=main`, a
30-second startup deadline and a 10-second watchdog. The main process emits `READY=1` only
after configuration and deployment validation, exclusive state ownership, SQLite initialization,
a successful read through the operational worker, management listener binding and router
construction. A notified invocation without deployment storage fails closed. Ordinary local
CLI invocations without `NOTIFY_SOCKET` keep working without systemd.

The service loop polls the management server and completes a new `PRAGMA schema_version`
request through the same bounded queue and worker used for operational writes before each
`WATCHDOG=1`. It waits half of `WATCHDOG_USEC` between probes and bounds each probe by that
same duration. Only one probe can be outstanding; missed ticks are not accumulated. A full
queue, stopped worker, failed database read or missed progress deadline ends service operation
with failure. No separate timer or cached health flag can acknowledge a probe. A stalled
service loop cannot send notifications even when other runtime threads are alive. Notifications
use nonblocking Unix datagrams, supporting pathname and Linux abstract sockets. A configured
but unavailable notification socket fails startup; a runtime notification failure stops service.
`WATCHDOG_PID`, when present, must identify this process for watchdog emission.

This measures the currently composed management/service loop and authoritative storage worker.
Charging station workflows are not yet composed into this executable. Their eventual owning
loops must participate in the progress gate when added; this is not evidence that those future
workflows are live. Target or internet availability is not a watchdog requirement.

Normal termination sends `STOPPING=1`, closes ingress and drains within the existing shutdown
budget. A process hang is handled independently by systemd: watchdog expiry aborts the process,
with `TimeoutAbortSec=5s` bounding escalation to forced termination. Core dumps are disabled.
`Restart=on-failure`, `RestartSec=5s`, and five starts per 60 seconds bound rapid restart loops;
manual stops do not restart. This rate limit is not a lifetime failure counter or an artifact
rollback policy.

Each service's `ExecStopPost` writes a `uob_service_exit` journal message containing systemd's
`SERVICE_RESULT`, `EXIT_CODE`, `EXIT_STATUS`, and `INVOCATION_ID`. This retains each invocation's
termination cause before restart replaces the current status. An initialization failure before
the main process starts can have empty code/status values. The normal production/staging journal
namespaces and retention bounds still apply. Release supervision can inspect the per-invocation
journal records and systemd's `Result`, `ExecMainCode`, `ExecMainStatus`, and `NRestarts` properties;
no new privileged command interface or artifact selection is introduced. The independent release
manager itself remains unimplemented.

The notification and termination semantics follow the upstream
[systemd service implementation](https://github.com/systemd/systemd/blob/main/src/core/service.c).

## Verification

`./scripts/verify-workspace.sh` includes real Unix-datagram process tests for readiness,
notification progress, SIGSTOP/SIGCONT, shutdown and rejected startup. Storage tests stall the
real operational worker behind a test-owned gate, demonstrate a probe timeout, then release
it and verify progress resumes. Another test keeps an unrelated timer active while the storage
probe stalls. Production code has no fault-injection environment variables or endpoints.

After building `target/debug/uob`, CI runs:

```text
python3 -B scripts/test-service-watchdog.py --disposable
```

Run this only on a disposable Linux host with systemd and root/sudo. It creates a unique
transient service using the shipped readiness/restart/watchdog settings and private temporary
state. It verifies healthy operation across a watchdog period, suspends the process to trigger
watchdog termination, then injects crashes to prove the restart limit and per-invocation causes.
It stops and removes only its test service; it never installs or controls the real UOB units.
