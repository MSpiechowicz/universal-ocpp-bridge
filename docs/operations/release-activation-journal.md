# Durable release transitions and activation ownership

The release manager owns an `ActivationJournal` for its canonical artifact store
before serving IPC. A second manager cannot obtain activation ownership for that
store even if it uses a different supervisor state or socket directory. The
`.activation-owner.lock` file is never replaced or unlinked; its exclusive kernel
lock lasts until the owner exits. Every journal transaction also takes the existing
`.disk-admission.lock`, excluding concurrent installation and pruning.

The journal is a persistence primitive for the release policy, not an additional
operator authorization interface. Public `promote` and `rollback` requests still
return `qualification_required`. Qualification evidence (#154), production
configuration/backup and idle admission (#155–#156), process control (#157), and
probation/rollback policy (#158–#160) supply those decisions. In particular, merely
calling `Qualify` or `MarkHealthy` in internal code does not prove a soak or a
24-hour production probation. No journal operation executes a process.

## State and transition rules

`.release-state.json` is a bounded, mode-0600 snapshot containing a monotonic
sequence, production digest/phase, previous-good digest, and one candidate
digest/phase. It contains no operational database, credentials, customer data,
commands or exports. The state path is fixed inside the owner-controlled artifact
store, so choosing another private IPC state directory cannot create a second
activation authority.

The candidate proceeds through installed, staging, qualified, promoting, probation
and healthy. A failed staging/qualification observation quarantines only the
candidate; production state and both production pointers remain unchanged. The
journal rejects skipped transitions, stale candidate observations and invalid
digests. Installing a replacement requires a new installed observation and discards
its predecessor's qualification. Installation cannot replace the candidate slot
during promotion, probation or rollback.

Beginning promotion retains the former healthy production artifact as
`previous-good` and selects the candidate as `active`. Entering probation and
healthy records policy observations without changing the retained fallback.
Rollback quarantines the activated candidate and selects the retained previous-good
artifact with phase rolling-back; completion records previous-good. The journal
never rewinds local or exported data. This primitive covers one candidate lifecycle;
policy orchestration decides when a later installation may replace that candidate.

On first use, existing pointers are verified. An `active` pointer alone initializes
as installed, never as healthy. An active digest already explicitly designated by
the `previous-good` pointer initializes as previous-good. Empty stores have no
production artifact. Initial deployment authorization remains a policy concern.

## Commit and recovery protocol

For each transition, while holding both ownership and store locks:

1. Revalidate pointer/journal agreement and installed bytes against current trust,
   host, schema and security policy. Reject a replaced candidate.
2. Write the complete before/after intent to `.activation-intent.next`, fsync it,
   rename it to `.activation-intent`, and fsync the store directory.
3. Write each changed digest pointer to its own `.next` file, fsync, rename and
   fsync the directory. Preserve the previous-good reference before switching active.
4. Atomically replace and sync `.release-state.json` using the same procedure.
5. Remove the intent and sync the directory before acknowledging completion.

Before the intent is published, scratch files have no authority. After publication,
recovery completes the exact recorded target. An interruption during recovery uses
the same rule. Recovery checks both snapshots, the permitted transition, sequence,
candidate identity, installed target bytes, and each pointer against the old or new
value. It rejects unrelated pointer changes, malformed committed metadata, unsafe
links and revoked/corrupt targets. Incomplete unpublished scratch files can be
removed only after validating they are owner-controlled regular files. Committed
malformed evidence is preserved for administrator inspection, never silently reset.

Installers refuse access while an intent or unpublished intent scratch remains;
recovery takes the same store lock but skips pruning until the transaction is
consistent. An I/O error poisons the live journal: drop and reopen it before any
further transition. No retry can accidentally start a second transition over a
partially completed one.

The lifetime activation lock serializes control ownership, including restart
recovery. The eventual fixed-unit service adapter must hold that owner throughout
stop/switch/start, confirm the old production unit is stopped before beginning
promotion or rollback, and use the service's existing state/runtime process locks.
The journal alone is not a service launcher and cannot attest that a daemon has
started or stopped. These tests make no live-systemd or physical power-loss claim.

## Verification

`cargo test --locked -p uob-release-manager` injects interruption after file creation,
write, fsync, rename and directory sync, and around intent cleanup for every lifecycle
transition and rollback recovery. Reopening must expose the old or fully committed
new state and a matching verified production artifact. Tests also cover staging
failure isolation, expired qualification after replacement, candidate-slot retention,
malformed intents, hostile scratch symlinks, competing owners, and kernel lock release
after killing a real owner subprocess. Existing service deployment tests separately
exercise duplicate production-process rejection through its state/runtime locks.

These are deterministic process/I/O interruption tests of the durability ordering,
not a filesystem power-cut emulator or the broader activation fault suite (#169).
The bounded snapshot and one in-flight intent are not a historical audit stream;
sanitized audit delivery remains #163.
