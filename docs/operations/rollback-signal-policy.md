# Persistent release failure classification

The independent supervisor accepts typed host observations through
`Supervisor::observe_failure`. This is a trusted Rust host boundary, not a new IPC operation:
read, stage and activate credentials cannot invent process deaths or change failure thresholds.
The input must come from the host's service-manager observation owner, with a durable increasing
observation ID, UTC seconds and increasing service invocation IDs. Supervisor reboot must resume
that cursor and the current invocation; it must not invent a new application start. Repeated
termination reports for one invocation count once. An unexpected clean exit while desired-running
uses the same `Exit` signal as any other unexpected exit. Planned stops do not count.

The policy is persisted on first observation and cannot change silently within the incident.
Defaults are:

| Evidence | Decision |
| --- | --- |
| Startup remains not core-ready at 30 seconds | Rollback required |
| Three distinct unexpected exits, watchdog or OOM terminations in an inclusive 120-second window | Rollback required |
| Three consecutive internal readiness failures at 10-second intervals, after a 30-second startup grace | Rollback required |
| Confirmed fatal invariant with valid, compatible data | Immediate rollback required |
| MQTT, EMS or external-database outage, credential rejection, malformed charger traffic, no chargers | Component degraded |
| Invalid/incompatible data, corrupt/full storage, OS/kernel failure, both versions failing | Critical recovery required |

The host must sample startup and readiness even if no IPC clients are connected. Faster readiness
observations do not inflate counters; a missed scheduled sample breaks the consecutive sequence.
A core-ready observation clears readiness failures and prevents subsequent startup-timeout signals
from counting. Exit history is retained across service restarts and supervisor reboot. Backward
UTC observations and repeated/out-of-order IDs fail closed, requiring the host to resolve clock or
cursor discontinuity rather than clearing evidence. Durations are configurable from 1 to 86,400
seconds (grace may be zero); counts are bounded from 1 to 64.

When resource pressure contributes to an internal failure, the supervisor first persists a
`stop_staging` intent. Its injected `StagingStop` host operation must stop the fixed entire
`uob-staging.slice`, use a bounded deadline and confirm inactivity. It accepts no paths, commands
or unit names from callers. A successful stop clears the readiness streak and requires fresh
readiness evidence. Termination and confirmed fatal-invariant evidence is classified after the
stop, since a dead process cannot provide another readiness sample. An unavailable or unsuccessful
stop exposes critical recovery. An interrupted stop remains pending across reboot and is retried
idempotently before classification. The host must keep staging stopped for that incident.

The private supervisor ledger atomically persists the counters, staging intent, policy, latest
64 sanitized observations and a separately retained terminal trigger. Its input/output size is
bounded to 64 KiB. No raw exception, endpoint, payload or credential text is accepted. An interrupted
ledger publication blocks mutation, including staging side effects, and exposes recovery status.
Authenticated status reads include `failures`; critical recovery also returns `recovery_required`
and blocks release mutations. Rollback/recovery decisions remain latched across reboot; new ready
signals cannot erase an incident or cause repeated version selection.

The trusted host can connect this classifier directly to the guarded artifact switch through
`Supervisor::observe_failure_and_rollback`; see [automatic rollback](automatic-rollback.md).
The executable does not yet collect systemd termination events or connect a production staging-stop
driver to that boundary. Host composition must supply authoritative observations and fixed staging
control before claiming automatic on-device failure handling.

Run `cargo test --locked -p uob-release-manager --test failure_policy` for threshold, reboot,
degradation, resource-pressure ordering and interrupted-write evidence. The full workspace verifier
also runs the existing real IPC and activation tests.
