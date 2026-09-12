# Production artifact activation

`Supervisor::promote_at_idle` implements issue #157 after signed qualification and production
preflight. The caller supplies a kernel-authenticated UID and a trusted `promotion::Host`: the
**live production worker's** drain port, a staging-slice stop adapter, the production process
controller, and a bounded maintenance window. The supervisor applies its existing `Activate`
grant; none of these host connections or paths are accepted from a public request body.

The synchronous IPC handler remains fail-closed when no live host is attached. The current
standalone service composition does not yet transport the process-local drain port over IPC.
Do not work around that boundary by opening another SQLite worker or passing an idle snapshot.
The activation entry point is available to a host that owns the real live drain connection;
installing the systemd template alone does not enable promotion.

## Ordering and durable evidence

1. Revalidate the exact qualified candidate, current security/compatibility policy and production
   configuration, and create the existing separate, consistent preflight backup.
2. Drain the real production worker, wait for charging, unresolved commands and stateful jobs,
   stop staging, and seal the final idle revision. Recheck qualification after the wait.
3. Persist a bounded `Stopping` attempt in the private supervisor ledger before process control.
   It binds candidate, previous artifact, configuration digest, production input policy and the
   production database's device/inode. No credentials or raw paths enter the public status.
4. Revalidate the seal and configuration, then stop and confirm the **complete** old production
   process group within the remaining maintenance window. Queued stops, errors, cancellation,
   expired windows and changed qualification cannot authorize a pointer switch.
5. Recheck production inputs. Commit `BeginPromotion` through the existing fsynced intent and
   atomic pointer journal, then record `Starting` before starting the verified candidate.
6. Start using the existing production configuration, service UID/GID and operational database.
   Wait at most 30 seconds for core readiness, then enter `Probation`. Startup success never
   establishes the 24-hour healthy/known-good observation.

The activation journal owner remains held across all steps and excludes other supervisors.
The independently owned service state/runtime locks exclude competing daemons. Activation has
no interface for copying staging state, restoring a backup, repointing pending deliveries,
rewinding export IDs/checkpoints, or emitting charger commands. New connections use the existing
transaction and uncertain-command recovery paths; protocol acknowledgement remains distinct from
observed physical effects.

## Interruption and failure

A dropped activation future leaves its durable phase visible. On reopening, the activation
journal first recovers any published pointer intent. `recover_activation` requires the same
`Activate` permission class, persists a one-shot recovery attempt, stops the entire production
group again, validates the unchanged production inputs and verified selected binary, then starts
**the artifact currently owned by the journal**.

If interruption preceded pointer intent publication, only the old artifact resumes, the ledger
reports `RecoveredPrevious`, and another promotion requires explicit operator recovery and fresh
preflight/drain. If the intent selected the candidate, recovery resumes that candidate and enters
probation. It never reconstructs or reuses the old process-local seal. Changed configuration,
replaced database, unsafe metadata, a failed stop/start, or a second interrupted recovery leaves
`RecoveryRequired`; it does not loop through versions. Automatic failure-driven rollback and
probation completion remain the separately owned failure-policy workflows.

The existing preflight backup slot is retained on failed/deferred attempts. It requires the
existing explicit retention handling before another preflight; activation never silently deletes
that recovery evidence. A first installation without an independently qualified previous-good
artifact remains outside normal reversible promotion.

## Linux process adapter

`SystemdProduction` is a narrow adapter for administrator-installed units. It stops
`uob-production.slice`, including the existing `uob.service` and any selected artifact instance,
then requires cgroup v2 evidence that the slice is empty or removed. It starts only
`uob-production@<verified-64-character-digest>.service`, with the packaged `Type=notify` template.
The blocking systemd start job must complete; `--no-block` is never used. Cancelling the systemctl
helper does not prove PID1 cancelled its job, so recovery always stops the slice again.

The template fixes the binary at `/var/lib/uob-releases/artifacts/%i/bin/uob`, production
configuration at `/etc/uob/bridge.toml`, state at `/var/lib/uob`, and runtime at `/run/uob`.
The adapter rejects a mismatched binary, configuration, database, UID/GID or configuration digest.
The unit retains the existing watchdog, stop deadline, control-group kill behavior, filesystem
isolation and production resource limits. Unit files and start authority must remain
administrator-controlled. Do not enable an artifact instance independently at boot: the host
activation owner must resolve the durable journal before selecting a production instance.

This change does not relax the packaged supervisor sandbox or deploy units on a running host.
Host integration must provide the live drain transport and narrowly scoped process-control and
production-preflight access; a missing connection keeps normal IPC promotion blocked.

## Verification

`cargo test --locked --package uob-release-manager` covers the activation coordinator alongside
the existing journal fault sweep, exclusive ownership, qualification, drain and preflight tests.
The new activation tests use actual subprocesses owning a socket and service lock and a real
SQLite store: they verify stop-before-start, reconnect to the same endpoint, preserved database
inode, monotonically allocated transaction IDs and original pending-delivery identity, unchanged
configuration, permission rejection, stop errors/deadlines, startup failure, configuration drift,
and one-shot recovery on both sides of the pointer switch.

The subprocess fixture is test infrastructure, not an OCPP charger or a full production service.
Existing storage/protocol recovery tests provide the transaction and uncertain-command behavior;
the activation controller adds no replay path. A privileged systemd/Pi activation is not claimed
by these hardware-free tests.
