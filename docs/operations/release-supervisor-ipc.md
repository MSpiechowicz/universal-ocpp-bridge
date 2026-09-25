# Independent release supervisor and local IPC

`uob-release-manager serve CONFIG` runs independently of the bridge, its management
listener, and its browser. Its first independent package version is **0.1.0**;
`--version` reports that version. Application workspace version bumps no longer
change it. The existing offline `install` and `verify` commands remain available.
The [`uob release` commands](headless-cli.md#independent-release-control) provide the operator
client without loading bridge configuration or depending on the HTTP API.

The standalone supervisor owns the release authorization and persistence boundary.
`stage` revalidates an installed candidate and persists `staged_verified_digest`;
it does **not** assert that staging ran. `qualify` verifies signed harness evidence
under the [qualification gate](release-qualification.md). Unqualified public
promotion returns `qualification_required`; qualified promotion runs
[production preflight and backup](release-preflight-backup.md), returning
`preflight_rejected` on failure or `activation_blocked` without a live drain/process
host. Public rollback remains `qualification_required`. The
[activation coordinator](production-artifact-activation.md) and
[one-attempt automatic fallback](automatic-rollback.md) are internal trusted-host
paths, not connected to this standalone IPC service; the systemd observation collector
is not installed. The [release recovery runbook](release-recovery-runbook.md)
distinguishes those paths from operator commands. Client permission is never
qualification or a process-control capability.

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

Follow the [chronological installation](operations-runbook.md) to verify independent
status while the application is stopped. The platform CI application tarball does
not include this independently provisioned supervisor executable.

Only deliberately authorized local operator accounts should join the IPC group.
Group membership allows connecting; it does **not** grant an operation. Configure
each operator's numeric UID and exact permissions in `grants`. The optional
[management read bridge](management-read-api.md#release-supervisor-read-bridge) may
add the production bridge account to this group with **only** `read` permission.
Never grant it `stage` or `activate`, and never add the staging account. Root is
explicitly listed in the example and has no special protocol bypass. Restart the
supervisor after changing grants or trust/revocation policy; those administrator
files are loaded at startup. Restart does not discard persisted supervisor state.

The standalone service has no attached production process-control backend or
service-control privileges. Its writable paths are its own state/runtime directories
and the approved artifact store. The application and staging state/configuration
are inaccessible by default; the optional preflight drop-in permits read-only
production access. A future connected activation host must retain a fixed
application-service allowlist and all qualification gates; arbitrary units or
executable paths are not supported.

## Local protocol v1

Connect to `/run/uob-release-manager/control.sock`, a mode-0660 Unix stream socket.
Send exactly one newline-terminated JSON object per connection and read one JSON
response line. Example requests:

```json
{"operation":"status"}
{"operation":"events","after":0}
{"operation":"stage","digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
{"operation":"promote","digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
{"operation":"rollback"}
```

| Operation | Required permission | Current effect |
| --- | --- | --- |
| `status` | `read` | Read private supervisor evidence without querying the bridge |
| `events` | `read` | Snapshot up to 64 retained operator outcomes and supervisor decisions after an exclusive sequence cursor, with oldest/latest sequence and truncation metadata |
| `stage` | `stage` | Under the artifact-store lock, require the current candidate and reverify its signature, host/security/schema eligibility, ownership, sealed layout and bytes; persist verification evidence |
| `qualify` | `stage` | Verify signed evidence from the private inbox and persist an exact candidate/evidence reference |
| `promote` / `rollback` | `activate` | Record `qualification_required` (or `preflight_rejected` / `activation_blocked` after qualified production preflight); leave application pointers and services unchanged |

Permissions are independent. Read does not grant stage or activate; stage does
not grant read or activate; activate does not grant read or stage. Linux
`SO_PEERCRED` supplies the peer UID. JSON cannot provide a UID, permission, path,
unit, command, trust key, or supervisor-update operation. Unknown operations and
fields fail validation, including extra fields on status and rollback. Digests
must be exactly 64 lowercase hexadecimal characters.

Responses contain `protocol: 1`, `manager_version`, a safe `code`, and, only for
authorized read requests, `status` or `events`. Codes are `ok`, `forbidden`,
`invalid_request`, `busy`, `artifact_rejected`, `qualification_required`,
`evidence_rejected`, `preflight_rejected`, `activation_blocked`, `recovery_required`, and `storage_failure`. No raw request or OS error text is
echoed. Status reports a monotonic audit sequence, failed authorized operator request
count, last operator outcome and authenticated UID, and the last successfully verified
staging digest. Internal decisions advance the sequence without replacing `last_operation`.
That digest is a historical observation, not a current
qualification or activation permission. It is cleared by a failed staging check.
An events response contains `records`, `oldest_sequence`, `latest_sequence`, and `truncated`.
Omitting `after` selects zero; records have sequence strictly greater than the cursor.
An empty history reports both bounds as zero. Reads remain available during recovery and do
not append records or change state. Unauthorized reads disclose neither status nor events.

Each event has `sequence`, `uid`, `request`, `result`, and `actor`. Operator records
use the kernel-authenticated peer UID and `actor: operator`. Internal decisions use
the supervisor's effective UID and `actor: supervisor`, with a typed `decision`:

- Promotion: exact candidate, previous-good, qualification evidence and configuration
  digests when available; compatibility, drain, health and outcome categories.
- Failure: trusted observation ID/time, closed signal category and resource-pressure
  flag, artifact identities, policy decision and the pinned incident trigger ID.
- Rollback: quarantined and previous-good digests, attempt/restored/recovery-required
  step and a closed reason category.

No credential, secret reference, configuration contents, path or raw error is an
audit field. Promotion/rollback requests remain subject to the existing gates;
the event API cannot initiate them. Unauthorized and malformed requests rejected
before recording do not consume retention capacity.

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

The private ledger retains at most 64 audit records, the last operator outcome,
aggregate counters, and current promotion, probation, failure and rollback state.
Oldest events are also evicted when needed to keep the **combined** encoded state
within 64 KiB; current incident context is never evicted to make room for history.
`truncated` signals an expired cursor. Archive exports externally when a complete
long-term audit trail is required. The ledger is outside application bundles and
the operational database; application rollback does not roll audit evidence back.

Loading older ledgers treats records without `actor` as operator records. If an
older ledger retained only its last operation, earlier cursors report truncation.
Each recorded transition and its audit event commit together by writing `state.next`,
syncing it, renaming it to `state.json`, and syncing the directory. Unauthorized
operations cannot allocate records or touch the artifact store. Sequence exhaustion,
an oversized pinned status and persistence errors fail closed.

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
