# Independent OCPP 1.6J diagnostics and log station model

`uob-sim` can act as an OCPP 1.6J charge point that uploads diagnostics and logs for a CSMS. The
model is derived independently from the pinned OCA sources, not from the bridge implementation:

- OCPP 1.6 Edition 2 §4.4, §5.9, §6.17/6.18 and §6.25/6.26 (`GetDiagnostics` and
  `DiagnosticsStatusNotification`).
- OCPP 1.6 Security Whitepaper Edition 4, use case N01 and the `GetLog` and
  `LogStatusNotification` messages.

A station may implement either family or both. `legacy` enables `GetDiagnostics` and
`security_log` enables `GetLog`; at least one must be on. A family that is off is answered with
CALLERROR `NotImplemented`. Stations without a `diagnostics16` table keep the client library's
`NotImplemented` reply for both actions.

## Configuration

```toml
[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/ocpp/alpha"
ocpp_version = "1.6"

[stations.diagnostics16]
private_state_file = "/srv/sim/private/diagnostics.json"  # required, owner-only like reservation16
legacy = true                   # GetDiagnostics / DiagnosticsStatusNotification (default true)
security_log = false            # GetLog / LogStatusNotification (default false)
maximum_bytes = 1048576         # 1..=67108864 per upload, default 1 MiB
upload_timeout_ms = 30000       # 1..=300000 per attempt
status_delay_ms = 0             # 0..=30000 before each status CALL
diagnostics_bytes = 4096        # size of a generated diagnostics file, 1..=maximum_bytes
security_log_bytes = 2048       # size of a generated security log, 1..=maximum_bytes
cancel_policy = "cancel"        # GetLog during an upload: "cancel" (N01.FR.11) or "reject"
# Fault controls:
no_diagnostics = false          # legacy only: reply without fileName and upload nothing
upload_failures = 0             # 0..=16 initial attempts that fail without network access
reject_get_log = false          # security_log only: reply Rejected
get_log_failure_status = "UploadFailure"  # security_log only: or BadMessage, PermissionDenied,
                                          # NotSupportedOperation (N01.FR.10)
```

The private state file and its `.lock` use the same owner-only, exclusive, fsync-before-rename
storage as the reservation and firmware models. The table is rejected for an OCPP 2.0.1 station;
see [diagnostics201.md](diagnostics201.md) for the 2.0.1 `GetLog` model.

## Station behavior

Both families validate the native payload strictly. Unknown fields, a JSON `null` for an optional
field, a missing required field or a wrong type return CALLERROR `FormationViolation`. These
return `PropertyConstraintViolation`:

- a negative or out-of-`int32` `retries`, `retryInterval` or `requestId`;
- an empty location, one that is not an absolute URI, or a `remoteLocation` above 512 characters;
- an invalid RFC 3339 date-time, or a time window that ends before it starts;
- a `logType` other than `DiagnosticsLog` or `SecurityLog`.

`GetDiagnostics` (§5.9) is answered `{"fileName": "diagnostics-<station>-<n>.log"}`. A station
configured with `no_diagnostics`, or one already uploading, answers `{}` without a file name and
starts nothing; OCPP 1.6 defines no cancellation for this message. The station then sends
`DiagnosticsStatusNotification` `Uploading` and uploads one file. Each failed attempt is retried up
to `retries` times, waiting `retryInterval` seconds (capped at 60), and `Uploading` is reported
again for each attempt. The station finishes with `Uploaded`, or `UploadFailed` once every attempt
failed.

`GetLog` (N01.FR.01) is answered `{"status": "Accepted", "filename": ...}`. The file name is
`diagnosticslog-<station>-<n>.log` or `securitylog-<station>-<n>.log` by `logType` (N01.FR.03/04).
`reject_get_log` answers `Rejected`. A `GetLog` that arrives during any ongoing upload follows
`cancel_policy`:

- `cancel` stops the ongoing upload and answers `AcceptedCanceled` (N01.FR.11). The cancelled
  upload sends no further status, even when its transfer completes later.
- `reject` keeps the ongoing upload and answers `Rejected`.

Every `LogStatusNotification` carries the `requestId` of the `GetLog` that started the upload
(N01.FR.07). The station reports `Uploading` for each attempt (N01.FR.08) and `Uploaded` on success
(N01.FR.09). Once every attempt failed, it reports the configured `get_log_failure_status`
(N01.FR.10).

Uploads use the bounded `artifact_transfer` client with HTTP PUT. When the location ends with `/`,
the file name is appended (N01.FR.21). Only `http` and `https` locations without embedded
credentials are used; any other location fails the attempt. The generated content starts with
`UOB-SIM DIAGNOSTICS LOG` or `UOB-SIM SECURITY LOG`, names the station, job, file and requested
time window, and is padded to the configured size. It never contains the location.

A `TriggerMessage(DiagnosticsStatusNotification)` reports `Idle` unless a `GetDiagnostics` upload
is active, in which case it reports `Uploading` (§4.4). A `GetLog` upload is reported only through
`LogStatusNotification`.

## Durability and delivery

Each transition is committed to the private state before its status CALL is attempted. Statuses
leave the durable outbox in order, one at a time, and only on a socket whose `BootNotification`
was accepted. A CALLRESULT, a CALLERROR or an undecodable reply counts as delivered. A timeout or
transport failure is retried after one second. Delivery is at least once.

After a process restart, an interrupted upload starts that attempt again and reports `Uploading`
again. A waiting retry continues to wait. A configured diagnostics model also makes automatic
reconnects re-register before any native call.

## Scenario actions

`assert_diagnostics` and `await_diagnostics` compare an `expect_response` subset of the safe
snapshot:

- `stateAvailable`, `legacy`, `securityLog`, `active`;
- `kind` (`Diagnostics`, `DiagnosticsLog` or `SecurityLog`), `requestId`, `fileName`, `attempts`;
- `lastStatus`, `statuses` (statuses delivered for the current or last job, in order);
- `uploads` (completed uploads), `pendingStatuses` and `cancelled` (cancelled `requestId`s).

The event name is `diagnostics_observed`. The snapshot, traces and debug output never contain
locations or log content. See `bins/uob-sim/examples/diagnostics-1.6*.toml`.

## Not modeled

- **`ExtendedTriggerMessage`.** It is not implemented, so a triggered `LogStatusNotification`
  without `requestId` (N01.FR.12) is never sent.
- **`AcceptedCanceled` status notification.** N01.FR.20 names it, but the pinned
  `UploadLogStatusEnumType` schema does not contain it, so a cancelled upload reports nothing.
- **FTP/FTPS, POST uploads and resume.** Uploads use HTTP(S) PUT only (N01.FR.14/18/19/22/23).
- **Security events.** No security log entries are recorded for events (N01.FR.05, issue #127).

## Verification

```text
cargo test --locked -p uob-sim --test diagnostics16_model --test diagnostics16_wire \
  --test diagnostics16_process --test diagnostics16_scenario --test diagnostics16_independence
```

`diagnostics16_independence` checks that the simulator reaches no bridge crate. It also checks
that every emitted legacy status is in the pinned corpus schema enum, and that `GetLog` replies and
statuses stay within the hand-transcribed Whitepaper enumerations.

The model tests cover both families, retries, delays, failure statuses, cancel policies,
unsupported families, strict validation, ordered delivery and configuration bounds. The wire tests
use a real OCPP-J socket and a loopback HTTP upload receiver:

- both families' uploads with byte-checked content and exact status sequences;
- an `Idle` trigger;
- refused uploads that retry and then fail with the configured status;
- a stalled upload cancelled by a new `GetLog` that then stays silent;
- `NotImplemented` for a disabled family.

The process test kills the real binary mid-upload. It checks that the restarted binary stores the
file once and reports exactly one `Uploaded`, and that no upload location reaches stdout or
stderr.
