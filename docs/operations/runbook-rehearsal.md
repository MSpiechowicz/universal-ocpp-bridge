# Disposable installation and recovery rehearsal

Rehearse only on a disposable Linux development/CI host with no production
charger, state or credentials. Use checked-in tests and temporary stores;
**never** run privileged isolation/systemd scripts against a charging host.
This checklist identifies evidence to capture when run: commands below are
**instructions, not claims of execution for this runbook**. The fixtures do
not install a privileged live production systemd service or prove live Pi
performance, continuous 24-hour soak, charger compatibility or public rollback.

## 1. Installation and independent status boundary

On an authorized clean disposable systemd host, follow
[production installation](operations-runbook.md) with separately verified
binaries, host-specific owner-controlled trust policy and an empty dedicated
store. Confirm the non-root production PID, root/group-owned supervisor socket,
management-loop health, journal namespace, bounded stop and
`uob release status --format json` **before and after**
`systemctl stop uob.service`. Record UID, unit `ActiveState`, manager
`protocol`/`code`, timestamps and host environment. Passing
`systemd-analyze verify` alone is not installation evidence. The repository's
`./scripts/test-environment-isolation.sh` and
`python3 -B scripts/test-service-watchdog.py --disposable` create disposable
identities/units and prove software isolation/watchdog behavior, **not**
that installation. The first script requires documented root/sudo, namespace
and tooling prerequisites; run it only on a disposable host.

Without host installation, the smallest focused independent IPC exercise is:

```sh
cargo test --locked --package uob-release-manager --test supervisor real_ipc_survives_supervisor_restart_without_a_bridge_process
./scripts/test-release-cli.sh
```

The first test uses a real Unix socket and separate supervisor subprocess
with no bridge. The CLI script builds `uob`, then runs ignored-but-explicit
CLI-to-supervisor cases (stage/qualify/status/events/permission and durable
audit after host-fixture rollback and bridge crash). Ignored `daemon_fixture`
and `cli_daemon` are subprocess entry points, not omitted production cases.
Capture exact exit status and bounded output; one case is not a whole suite.

## 2. Compatible application-failure drill

Start with the existing tests' disposable signed-artifact, qualification and
SQLite fixtures. The host-attached activation subprocess fixture reopens the
same database after promotion; the separate rollback fixture seeds
post-promotion committed records and switches back through an injected trusted
process controller. Neither fixture connects the standalone IPC manager to
a live production drain/collector. Execute:

```sh
cargo test --locked --package uob-release-manager --test production_activation activation_stops_old_socket_owner_and_reopens_production_data_in_probation
cargo test --locked --package uob-release-manager --lib rollback::tests::fallback_preserves_post_promotion_records_export_cursor_and_audits_across_reboot
cargo test --locked --package uob-release-manager --lib rollback::tests::trusted_observation_rolls_back_automatically_but_outages_never_stop_production
cargo test --locked --package uob-release-manager --lib rollback::tests::failed_or_interrupted_fallback_is_never_retried_after_reboot
```

Check test output for success and examine the assertions: the activation fixture
retains the database inode, next transaction ID and original pending-delivery
target/ID after candidate startup. The rollback fixture compares committed
transaction, meter, command, delivery, export-checkpoint and audit records
before/after fallback, preserves its SQLite bytes and quarantines the failed
digest across reboot. These are **separate fixtures**, not a demonstrated
live-host old→new→old charging cycle or measured external export cursor.
No backup/old database restore, reverse migration, charger command replay or
public CLI switch occurs. Record tested commit, toolchain, host OS, command,
exit status and assertions exercised. A failure leaves acceptance unproven.

For gate and storage/error boundaries, run these separate focused cases:

```sh
cargo test --locked --package uob-release-manager --lib rollback::tests::unsafe_eligibility_or_data_never_starts_a_fallback
cargo test --locked --package uob-release-manager --lib rollback::tests::missing_fallback_and_uncertain_ledger_are_actionable_without_process_calls
cargo test --locked --package uob-storage-adapter --lib backup::tests::backup_includes_committed_wal_and_survives_concurrent_writes
cargo test --locked --package uob-storage-adapter --lib backup::tests::current_validation_reads_wal_without_restoring_and_rejects_unsafe_inputs
cargo test --locked --package uob-release-manager --test preflight
cargo test --locked --package uob-release-manager --test qualification
cargo test --locked --package uob-release-manager --test failure_policy
```

The tests supply bounded signed fixtures and adversarial data. A fixture signature
or 24-hour timestamp is policy-validation input; **it does not establish a real
continuously run soak or live charger migration**. The storage backup cases prove
committed-WAL online copy and read-only current-data validation; the preflight
suite covers retained backup, refused overwrite and rejection without switching
production. Neither restores the current production database. Confirm the
public-command boundary separately with the CLI script above: promote can return
`activation_blocked` after successful qualification/preflight and rollback returns
`qualification_required`; do not mark either response as recovered service.

## 3. Full/corrupt storage drill without destructive production actions

On temporary test storage only, exercise a storage failure and follow
[critical recovery](release-recovery-runbook.md#4-fullcorrupt-storage-or-os-failure-preserve-evidence-first):
preserve source DB/WAL/identity and release ledger/pointers, record free
blocks/inodes, stop a failing fixture safely, and inspect a protected copy.
These focused cases verify refusal/preservation; they are **not** instructions
to corrupt a host's real filesystem:

```sh
cargo test --locked --package uob-release-manager --lib rollback::tests::unsafe_eligibility_or_data_never_starts_a_fallback
cargo test --locked --package uob-release-manager --test preflight
python3 -B scripts/test-disk-isolation.py --disposable
```

The last command requires privileges and uses disposable loop-mounted ext4
images in a private mount namespace. It proves staging ENOSPC cannot consume
production's separate capacity and retained artifacts. It requires the
script's root/sudo/disposable-host prerequisites and does **not** qualify
loop disks for deployed admission or measure a Pi. If unavailable, record
**not run**, not passed. Record observed copy/backup integrity, retained
data and actual operator recovery point; a backup's existence is not
permission to restore an older database during artifact rollback.

## Rehearsal record (fill only after execution)

- Host/disposable isolation, architecture, OS/systemd version, source revision and timestamp: _not run in this documentation change_.
- Production service UID, stopped unit state, supervisor running state, status `protocol`/`code` with bridge stopped: _not run in this documentation change_.
- Each exact command above: exit status, relevant observed assertion/output and retained artifact path in protected test records: _not run in this documentation change_.
- Live charger, production activation, hardware budgets, sustained soak and disaster-recovery restoration: **not established by repository fixtures**; require separately approved on-device/acceptance evidence.

A rehearsal is complete only when its recorded observations support all three
distinctions: independently queryable release status with stopped bridge,
compatible failure retaining **current** post-promotion data, and
corrupt/full-storage triage preserving evidence without automatic old restore.
