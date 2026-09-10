# Independent release supervisor and local IPC

`uob-release-manager serve CONFIG` runs independently of the bridge, its management
listener, and its browser. Its first independent package version is **0.1.0**;
`--version` reports that version. Application workspace version bumps no longer
change it. The existing offline `install` and `verify` commands remain available.

This issue establishes the supervisor ownership and authorization boundary. Staging
currently revalidates the installed candidate and persists `staged_verified_digest`.
It does **not** claim that a staging process ran or passed qualification. Qualification now verifies signed harness evidence; see the
[qualification gate](release-qualification.md). Promote and rollback require the
activation permission. Unqualified promotion returns `qualification_required`; a
qualified promotion returns `activation_blocked` pending production admission and
process control. Rollback remains `qualification_required`. The [activation journal and recovery state machine](release-activation-journal.md)
now persist internal policy transitions and recover pointer operations before IPC starts.
Production admission/activation (#155–#157) and
automatic rollback (#160) must supply their gates before those operations can
change a service or artifact pointer. Client permission is never qualification.

## Installation and independent lifecycle

Install the separately built supervisor binary at
`/usr/local/libexec/uob-release-manager`, owned by root and not writable by peers.
Application bundles allow only `bin/uob` and bounded static assets; a supervisor
binary, service unit, or install script in a bundle is rejected by the signed
artifact store. Application promotion cannot replace this executable.

The packaging files are:

- `packaging/systemd/uob-release-manager-sysusers.conf`: creates the local IPC group.
- `packaging/systemd/uob-release-manager.service`: independently enabled service,
  bounded restart, memory/task/file limits, private state, and Unix-only networking.
- `packaging/systemd/release-supervisor.json`: initial root-only authorization map.

Provision `/etc/uob-release-manager` as an administrator-owned directory. Install
the example configuration there as `supervisor.json` and provision the existing
signed-artifact `InstallPolicy` as `install-policy.json`. Both files must be regular,
administrator-owned, non-hardlinked, not writable by other users, and at most
64 KiB. The installer policy is checked against the **actual** host architecture,
OS ID and numeric OS version before the daemon listens. A host without a numeric
OS version is rejected, as it is by the existing offline installer.

Provision the canonical root-owned `/var/lib/uob-releases` artifact store following
[signed artifact installation](signed-artifact-store.md). Systemd creates separate
`/var/lib/uob-release-manager` (0700) and `/run/uob-release-manager` (0750), owned by
root with group `uob-release-control`. Install the sysusers definition and unit
through the host's administrator-managed package installation, then enable the
manager independently. There is no `Requires`, `PartOf`, or `BindsTo` dependency on
the application unit. Stopping the bridge does not stop release control.

Only deliberately authorized local operator accounts should join the IPC group.
Group membership allows connecting; it does **not** grant an operation. Configure
each operator's numeric UID and exact permissions in `grants`. Do not add the
bridge or staging service accounts. Root is explicitly listed in the example and
has no special protocol bypass. Restart the supervisor after changing grants or
trust/revocation policy; those administrator files are loaded at startup. Restart
does not discard persisted supervisor state.

The current service has no executable activation backend and no service-control
privileges. Its writable paths are its own state/runtime directories and the
approved artifact store. The application and staging state/configuration are
inaccessible. Adding future activation must retain a fixed application-service
allowlist and all qualification gates; exposing arbitrary units or executable
paths is not a supported extension.

## Local protocol v1

Connect to `/run/uob-release-manager/control.sock`, a mode-0660 Unix stream socket.
Send exactly one newline-terminated JSON object per connection and read one JSON
response line. Example requests:

```json
{"operation":"status"}
{"operation":"stage","digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
{"operation":"promote","digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
{"operation":"rollback"}
```

| Operation | Required permission | Current effect |
| --- | --- | --- |
| `status` | `read` | Read private supervisor evidence without querying the bridge |
| `stage` | `stage` | Under the artifact-store lock, require the current candidate and reverify its signature, host/security/schema eligibility, ownership, sealed layout and bytes; persist verification evidence |
| `qualify` | `stage` | Verify signed evidence from the private inbox and persist an exact candidate/evidence reference |
| `promote` / `rollback` | `activate` | Record `qualification_required` (or `activation_blocked` for qualified promotion); leave application pointers and services unchanged |

Permissions are independent. Read does not grant stage or activate; stage does
not grant read or activate; activate does not grant read or stage. Linux
`SO_PEERCRED` supplies the peer UID. JSON cannot provide a UID, permission, path,
unit, command, trust key, or supervisor-update operation. Unknown operations and
fields fail validation, including extra fields on status and rollback. Digests
must be exactly 64 lowercase hexadecimal characters.

Responses contain `protocol: 1`, `manager_version`, a safe `code`, and, only for
authorized status requests, `status`. Codes are `ok`, `forbidden`,
`invalid_request`, `busy`, `artifact_rejected`, `qualification_required`,
`evidence_rejected`, `activation_blocked`, `recovery_required`, and `storage_failure`. No raw request or OS error text is
echoed. Status reports a monotonic request sequence, failed authorized operation
count, last operation and authenticated UID, and the last successfully verified
staging digest. That digest is a historical observation, not a current
qualification or activation permission. It is cleared by a failed staging check.

The transport accepts at most four concurrent clients, 1024 request bytes per
client, and a one-second absolute read deadline. Trickle traffic cannot reset the
deadline. Replies also have a one-second write timeout; excess connections get
`busy` with a short write deadline. One owner serializes supervisor operations;
requests contending with verification receive `busy` rather than accumulating
an unbounded queue. The signed artifact-store limits still bound verification.

## Persistence and failure handling

A process-held owner lock prevents a second manager from using the same private
state. A separate runtime lock prevents competing socket owners. An owner-checked
stale socket left by process death can be removed after acquiring that lock;
regular files and links at the socket path are rejected without deletion.

The private ledger retains one bounded last-operation record and aggregate
counters; this is not a full audit history or the separate activation journal.
An authorized operation is acknowledged only after writing `state.next`, syncing
it, renaming it to `state.json`, and syncing the directory. Unauthorized operations
cannot allocate ledger records or touch the artifact store. Sequence exhaustion
and persistence errors fail closed.

An interrupted `state.next` leaves the previous `state.json` readable, reports
`recovery_required`, and blocks further mutations. Stop the supervisor and inspect
both files before explicit administrator recovery; no automatic merge, promotion,
or data restoration occurs. Corrupt committed state fails startup rather than
silently resetting counters. Neither file contains bridge SQLite data, customer
records, secrets, or downloaded code.

## Verification

`cargo test --locked -p uob-release-manager` covers the permission matrix, actual
kernel-authenticated Unix IPC in a separate process, supervisor death/restart with
no bridge process running, persisted UID/failure evidence, interrupted publication,
competing locks, altered candidate bytes, path/unit/shell/update injection, unknown
fields, and oversized/slow clients. The ignored `daemon_fixture` test is a child
process entry point explicitly invoked by those integration tests. It is not a
skipped acceptance case. Existing artifact tests also reject signed bundles that
attempt supervisor replacement. These tests require local Unix-socket access.

The systemd unit is checked as packaging; these tests do not claim a privileged
live-systemd installation, qualified staging execution, or production activation.
