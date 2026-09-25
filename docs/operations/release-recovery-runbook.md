# Release incident and disaster-recovery runbook

Use this decision path with an independently installed supervisor. Preserve
state before acting; a version change is **not** a storage repair. The public
`uob release` IPC reads status/events, stages and qualifies signed candidates
and requests promote/rollback, but the standalone manager cannot attach the
live process-local drain/activation host or systemd observation collector.
Qualified `uob release promote --release DIGEST` returns
`activation_blocked` after successful preflight. The public rollback command
(`uob release rollback --to previous-good`) returns
`qualification_required`. Do **not** treat either
request as a release switch, run a production artifact instance directly,
hand-edit pointers or claim automatic on-device fallback from this IPC unit.
The [host-attached activation coordinator](production-artifact-activation.md)
and [one-attempt rollback](automatic-rollback.md) are internal paths exercised
with disposable fixtures, not wired into this standalone public socket.

## 1. Triage without modifying production

Record UTC time, host/OS identity, bridge environment, current and previous
artifact **digests**, current service invocation, supervisor response and
exact incident trigger IDs. Keep credentials and raw station/customer data
out of tickets. As an authorized `read` operator (root in the example policy),
even when the bridge is stopped:

```sh
uob release status --format json
uob release events --format jsonl
systemctl show uob.service -p ActiveState -p Result -p ExecMainCode -p ExecMainStatus -p NRestarts -p MainPID -p InvocationID
journalctl --namespace=uob -u uob.service
systemctl show uob-release-manager.service -p ActiveState -p Result -p MainPID
journalctl -u uob-release-manager.service
curl --max-time 3 -i http://127.0.0.1:8080/health
curl --max-time 3 http://127.0.0.1:8080/metrics
```

An unavailable HTTP endpoint when the bridge is down is expected;
supervisor status must not depend on it. `events` is a finite snapshot of
at most 64 records; record metadata cursor and `truncated` flag. Use
`uob release events --format jsonl --after NUMBER` to retrieve strictly
newer records, not to follow a stream. Archive observations under incident
access controls before eviction. A timed-out mutation may have completed:
read status/events before retrying. Ledger `recovery_required`, interrupted
`state.next`, changed pointer or unknown database ownership requires
administrator investigation, not blind retry. The manager fails startup on
corrupt committed state; preserve private state/store for forensics.

Check `/health` separately for core/storage readiness, `accepts_new_sessions`,
required deliveries, uncertain commands and component state. HTTP 503 means
local startup/core/storage inability; logical journal capacity may refuse
new starts while preserving active-session completion **where charging is
composed**. Check physical free blocks/inodes and SQLite integrity separately
from the logical budget. Inspect cgroup pressure, per-invocation unit exits,
watchdog and journal retention. The packaged production service is
management/storage-only, not a production charging acceptance profile.
A target outage alone does not prove a stopped charging core. See
[health](health-readiness-metrics.md), [storage admission](../architecture/storage-retention-admission.md)
and [failure classification](rollback-signal-policy.md).

## 2. Classify before selecting an action

| Evidence | Decision |
|---|---|
| Qualified candidate with compatible **current** data/configuration, verified installed previous-good, and confirmed local application failure: startup remains not core-ready at 30 seconds; three distinct unexpected exits/watchdog/OOM in 120 seconds; three consecutive 10-second readiness failures after 30-second grace; or confirmed fatal invariant with valid data | Eligible for **one** bounded fallback only through the trusted host-attached rollback owner after its admission/security/data checks. Preserve post-promotion records in the live database. Current public CLI alone cannot execute this fallback. |
| Broker, EMS/SCADA, target, optional external database or network unreachable; charger absent/malformed; credentials rejected | Component degradation or authentication incident. Repair that component/credential or wait for recovery, keep durable queues/uncertain outcomes intact. Do not select an older binary to fix an external outage. |
| Full filesystem, corrupt or incompatible SQLite/WAL, changed database identity, unsupported schema, OS/kernel fault, both binaries failing, inconsistent clock/ledger | **Critical recovery**, not automatic rollback. Stop unsafe activity, isolate capacity/device/OS fault, preserve evidence, invoke site disaster-recovery procedure. No forced artifact switch or old-database restore as part of rollback. |
| First installation, no independently healthy `previous-good`, revoked candidate/fallback, missing/invalid signed old→new→old evidence, expired qualification, changed configuration or unsupported migrations | No normal reversible transition. Retain current evidence; escalate to a separately reviewed maintenance/restore plan rather than weakening trust/security floors or inventing a known-good version. |

If resource pressure contributes to a **locally observed** failure on a co-hosted
staging machine, the trusted host policy persists a stop-staging intent, confirms
the entire staging slice stopped, and requires fresh observations before
considering rollback. The installed supervisor has no service-manager collector
connected to that policy. To shed optional staging manually, stop
`uob-staging-governor.service` and confirm `uob-staging.slice` plus **all** test-peer
units are inactive; correctly configured peers bind to the governor. Never stop
production to shed staging. If staging is absent there is nothing to shed.
The [staging governor](staging-resource-governor.md) cannot stop production.

## 3. Compatible application failure: retain current data

A normal fallback switches **executable identity**, not data or configuration.
Verify previous-good's pointer and immutable signed bytes against current
trust/revocation floors. Check both manifests' read/write support for current
public, configuration, operational SQLite and external formats; require genuine
signed old→new→old evidence with post-promotion records and a previously healthy
previous-good artifact. Confirm the current configuration digest, database
device/inode, live SQLite/WAL integrity and fixed process authority. A missing
check is refusal, not permission to copy files. The
[compatibility policy](release-compatibility.md) requires additive migrations
and a lossless old-configuration view; there is no reverse migration.

Where a real trusted host is connected to
`Supervisor::observe_failure_and_rollback`, it persists the incident and
quarantines the failed digest **before** effects. It stops and confirms the
entire production group, rechecks the same current database (including WAL)
read-only and the verified previous executable, journals **one** pointer
switch, then starts that executable with the **current** configuration,
database and external state. Only then can startup read/write continuity
and health be observed. Do not delete WAL, overwrite
`/var/lib/uob/operational.sqlite3`, replace identity/lock files, restore a
preflight backup, rewind export checkpoints, acknowledge pending deliveries,
replay uncertain charger commands or start two versions against one database.
Preserve transaction IDs, post-promotion commits, pending deliveries and the
incident/quarantine record. An attempted or interrupted fallback is
`recovery_required`, not retried automatically. Even a successful fallback
blocks further mutations until separate explicit operator recovery; there
is no public reset bypass. A CLI policy response is not permission to
manually emulate this sequence. See [automatic fallback](automatic-rollback.md),
[activation journal](release-activation-journal.md) and
[service shutdown/recovery](service-lifecycle.md).

A [production preflight](release-preflight-backup.md) backup is private,
bounded, verified **disaster-recovery** input outside artifact pointers.
Online SQLite backup includes committed WAL while production can write.
It is not the post-promotion live database: restoring it for routine
rollback discards later transactions, commands, delivery IDs or export
checkpoints. A new preflight refuses to overwrite its one retained slot;
preserve/export the slot under site retention policy and explicitly clear
it only after review. Coordinate operational and optional external
database disaster recovery; preflight does not back up or contact PostgreSQL.

## 4. Full/corrupt storage or OS failure: preserve evidence first

1. Stop **new charging starts** through normal admission where available.
   If the service cannot safely progress, use a planned
   `systemctl stop uob.service` and allow its bounded drain. Do not repeatedly
   restart a failing worker or run an old binary on suspect data. Capture
   unit/journal, supervisor, mount, block/inode, storage/OS error and timestamp
   evidence on a separate protected destination, without disclosing secrets.
2. Keep `/var/lib/uob/operational.sqlite3`, its `-wal`/`-shm` sidecars,
   `identity.json`, `service.lock`, supervisor private ledger, signed artifact
   store/pointers and retained backup **unchanged** during triage. Abnormal
   shutdown may leave committed records in WAL; do not unlink sidecars or
   remove locks to bypass ownership. Preserve a consistent copy/snapshot
   under an administrator-reviewed procedure with safe access controls and
   adequate destination capacity. Do not free space via candidate install.
3. For ENOSPC, distinguish production, staging, release and journal
   filesystems. Repair the failed device or increase independently verified
   capacity; do not evict required deliveries, active transaction state,
   selected artifacts, reservations, backups or journal evidence reflexively.
   Staging on its own partition may be stopped without touching production.
   Inspect the source mount/device and errors before restart. Logical journal
   admission does not prevent physical disk exhaustion.
4. For corruption, invalid identity/schema, changed configuration, uncertain
   activation journal or failed OS, retain originals and investigate with
   SQLite/OS specialists on a **copy** after quiescing the writer. Verified
   backup restoration is separately approved disaster recovery with a chosen
   recovery point, data-loss reconciliation for later charging, target
   deliveries and external exports, and verified security/identity; it is
   **not** automatic or part of `rollback`. Resume only after integrity,
   ownership, free capacity, selected artifact, configuration and readiness
   checks. If neither version handles current data, remain stopped and escalate.

Offline charging depends on durable local authorization, not broker/Internet
reachability: active unexpired resource-scoped grants can decide locally;
unknown, revoked, expired or out-of-scope identities are denied. Remote
controls to offline stations are refused before durable queue admission;
ambiguous in-flight transmissions remain unresolved and must not be replayed
blindly. Charger hardware behavior during CSMS loss is site/device policy,
not guaranteed by this bridge. See [local authorization](../security/local-authorization.md),
[uncertain command recovery](../architecture/uncertain-command-recovery.md)
and [durable target delivery](../architecture/durable-target-delivery.md).

Rehearse software state preservation using the
[focused disposable rehearsal](runbook-rehearsal.md). That evidence is
not a privileged systemd or live charger recovery claim.
