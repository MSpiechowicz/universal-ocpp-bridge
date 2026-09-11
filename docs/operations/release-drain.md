# Release drain and the idle boundary

Normal promotion temporarily disables new bridge-issued starts while existing charging,
commands, and stateful jobs finish. The maintenance window is administrator-selected, greater
than zero and at most 24 hours, and uses monotonic time. Expiry defers promotion and restores
admissions without issuing a charger stop, deleting active work, or restoring an older database.

`ReleaseDrainPort` belongs to the application. The production composition root must share its
single `SqliteOperationalStore` worker with every management/target command coordinator, protocol
state owner, and release-drain client. Clones share the same authority; a separate database
connection or a second worker is not an authorized admission path. Existing production instance
locks exclude a second daemon. The port is trusted internal service control, not a new external
command authorization route.

## What the gate covers

The worker inspects the typed command operation independently of the caller's storage-purpose
hint. During drain it refuses canonical Start and all privileged OCPP submissions: a raw
RemoteStartTransaction, RequestStartTransaction, or a vendor/stateful operation must not bypass
maintenance admission. Ordinary canonical Stop and charging-limit commands remain available until
the final boundary freezes operational writes. Existing command responses continue to commit.
Rejected submissions receive the common Busy error and create no durable command or dispatch.
Existing command results remain readable through the normal query API.

A charger-reported transaction is an observation of potentially autonomous charging, not a
bridge-issued start. Such observations continue to commit during drain and invalidate idle.
Snapshot transactions in Pending, Active, Suspended, or Uncertain state prevent activation;
only Ended is idle. Every unresolved durable command also prevents activation. Inventory reads
all persisted station snapshots and commands, including disconnected/recovered stations, without
using a truncated recovery page as proof of idle. Corrupt state fails closed.

Firmware and certificate owners register a bounded stable job identity before dispatch and finish
it only on confirmed resolution. `release_jobs` is an additive SQLite schema-v6 table with at most
128 admitted jobs and 128-byte identities. Registration failures must prevent dispatch. Jobs
survive process restarts; neither command acceptance, disconnect, timeout, nor drain expiry
finishes a job. Late/recovered job registration is accepted during drain and invalidates idle.
Future firmware/certificate workflow implementations must use this port across their full
stateful lifetime. A previous binary must explicitly qualify schema-v6 compatibility before
normal rollback; this change does not claim that an arbitrary older binary is eligible.

## Supervisor sequence and race handling

`wait_for_idle_boundary` owns one process-local window, polls the durable inventory at most
10 times per second, and waits for idle. It then calls the trusted `StagingStopPort` to stop the
complete staging slice and confirm it is empty. Merely submitting a stop request is insufficient;
an error or timeout defers promotion. This port must not force-stop production charging.

After staging stops, the same storage worker checks the window, exact observed revision, and
current durable inventory before sealing. Any intervening operational write or job change
invalidates the old observation, even if the late work has already finished. Queued writes are
ordered with the seal. Before the seal they invalidate the observation; after it, operational
writes and job changes are refused until cancellation/expiry and invalidate the original boundary. Reads remain available. Staging
is left stopped if late state forces deferral; production work continues.

The returned `IdleBoundary` is an expiring process-local capability, not an artifact authorization
or a persistent idle flag. The activation controller must validate it immediately before stopping
the old production process, finish that stop inside its remaining window, and confirm the old
process is gone before switching artifacts. A failed/expired stop must defer activation. Dropping
or explicitly cancelling the boundary reopens admissions; a saturated cancellation queue still
has the independently enforced worker deadline. A cancelled supervisor future cannot leave a
permanent admission freeze. Old owners cannot cancel newer windows, and restart cannot revive
an old capability.

Process stop/start, artifact switching, and wiring this policy into the privileged activation
controller belong to #157; the existing public promotion route remains fail-closed with
`ActivationBlocked`. Staging-stop adapters must supply actual stopped-slice evidence there.
This policy adds no browser or alternate production command path.

## Deployment limits and evidence

Bridge admission cannot prevent a charger from starting locally or under its offline policy.
Configure charger autonomous-start behavior and the maintenance window for the installation.
Late reports during drain are retained and block promotion; after production shuts down,
reconnection/reconciliation must use current durable state and must never blindly replay starts.
An idle observation is not proof that an autonomous physical charger will remain idle.

Focused checks:

```text
cargo test --locked -p uob-storage-adapter --test command_deduplication
cargo test --locked -p uob-release-manager --test drain
cargo test --locked -p uob-application maintenance_preserves_readiness
```

These use real SQLite workers and injected staging-control ports to prove shared admission,
late-state invalidation, queued-write ordering, persistent job recovery, deadline restoration,
failed/hanging staging stop, and no boundary before confirmed staging stop. Full repository
verification includes these tests. No live charging hardware or production service is controlled.
