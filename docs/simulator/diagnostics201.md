# Independent OCPP 2.0.1 log station model

`uob-sim` can act as an OCPP 2.0.1 charging station that uploads log files for a CSMS. The model is
derived independently from the pinned OCA sources, not from the bridge implementation:

- OCPP 2.0.1 Edition 4 Part 2, use case N01 (Retrieve Log Information) and the `GetLog` and
  `LogStatusNotification` messages.
- The byte-exact `GetLogRequest`/`Response` and `LogStatusNotificationRequest`/`Response` schemas
  in `tests/ocpp-fixtures/corpus/schemas/2.0.1`.

A station with a `diagnostics201` table answers `GetLog` for both `DiagnosticsLog` and
`SecurityLog`. Stations without the table keep the client library's CALLERROR `NotImplemented`.
Unlike OCPP 1.6 there is no legacy `GetDiagnostics` message, so the table has no `legacy` switch.
The 1.6 model is described in [diagnostics16.md](diagnostics16.md).

## Configuration

```toml
[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/ocpp/alpha"
ocpp_version = "2.0.1"
reconnect = true                # statuses are delivered after the station registers again

[[stations.evses]]
id = 1
connectors = [1]

[stations.diagnostics201]
private_state_file = "/srv/sim/private/diagnostics.json"  # required, owner-only like reservation16
maximum_bytes = 1048576         # 1..=67108864 per upload, default 1 MiB
upload_timeout_ms = 30000       # 1..=300000 per attempt
status_delay_ms = 0             # 0..=30000 before each status CALL
diagnostics_bytes = 4096        # size of a generated diagnostics log, 1..=maximum_bytes
security_log_bytes = 2048       # size of a generated security log, 1..=maximum_bytes
cancel_policy = "cancel"        # GetLog during an upload: "cancel" (N01.FR.12) or "reject"
# Fault controls:
upload_failures = 0             # 0..=16 initial attempts that fail without network access
reject_get_log = false          # reply Rejected: the log is not available (N01.FR.05)
failure_status = "UploadFailure"  # or BadMessage, PermissionDenied, NotSupportedOperation
                                  # (N01.FR.10)
```

The private state file and its `.lock` use the same owner-only, exclusive, fsync-before-rename
storage as the reservation and firmware models. The table is rejected for an OCPP 1.6 station, and
`reject_get_log` cannot be combined with `upload_failures`.

## Station behavior

The native payload is validated strictly against the pinned `GetLogRequest` schema. Unknown fields,
a JSON `null` for an optional field, a missing required field, a wrong type or a `remoteLocation`
above 512 characters return CALLERROR `FormationViolation`. These return
`PropertyConstraintViolation`:

- a negative or out-of-`int32` `retries`, `retryInterval` or `requestId`;
- an empty location or one that is not an absolute URI;
- an invalid RFC 3339 date-time, or a window whose `latestTimestamp` precedes `oldestTimestamp`;
- a `logType` other than `DiagnosticsLog` or `SecurityLog`.

`GetLog` is answered `{"status": "Accepted", "filename": ...}` (N01.FR.01). The file name is
`diagnosticslog-<station>-<n>.log` or `securitylog-<station>-<n>.log` by `logType`
(N01.FR.03/04). `reject_get_log` answers `Rejected` with `statusInfo.reasonCode`
`NoLogAvailable` (N01.FR.05).

A `GetLog` that arrives while an upload is assembling or uploading follows `cancel_policy`:

- `cancel` stops the ongoing upload and answers `AcceptedCanceled` (N01.FR.12). The cancelled
  upload sends no further progress, even when its transfer completes later. It reports exactly one
  `LogStatusNotification` `AcceptedCanceled` carrying the cancelled upload's own `requestId`
  (N01.FR.20 and N01.FR.07), queued ahead of the new upload's statuses.
- `reject` keeps the ongoing upload and answers `Rejected` with `reasonCode` `UnableToCancel`.

Every `LogStatusNotification` carries the `requestId` of the `GetLog` that started the upload
(N01.FR.07). The station reports `Uploading` for each attempt (N01.FR.08) and `Uploaded` on success
(N01.FR.09). Once every attempt failed, it reports the configured `failure_status` (N01.FR.10).
Each failed attempt is retried up to `retries` times, waiting `retryInterval` seconds (capped at
60).

A `TriggerMessage(LogStatusNotification)` answers `Uploading` with the ongoing upload's
`requestId`, or `Idle` without any `requestId` when no upload is ongoing (N01.FR.13). A station
without a `diagnostics201` table answers a plain `Idle`.

Uploads use the bounded `artifact_transfer` client with HTTP PUT. When the location ends with `/`,
the file name is appended (N01.FR.21). Only `http` and `https` locations without embedded
credentials are used; any other location fails the attempt. The generated content starts with
`UOB-SIM DIAGNOSTICS LOG` or `UOB-SIM SECURITY LOG`, names the station, job, file and requested
time window, and is padded to the configured size. It never contains the location.

## Durability and delivery

Each transition is committed to the private state before its status CALL is attempted. Statuses
leave the durable outbox in order, one at a time, and only on a socket whose `BootNotification`
was accepted. A CALLRESULT, a CALLERROR or an undecodable reply counts as delivered. A timeout or
transport failure is retried after one second. Delivery is at least once.

After a process restart, an interrupted upload starts that attempt again and reports `Uploading`
again. A waiting retry continues to wait. A configured log model also makes automatic reconnects
re-register before any native call, so a status lost with a socket is repeated on the next one.

## Scenario actions

`assert_diagnostics` and `await_diagnostics` compare an `expect_response` subset of the safe
snapshot:

- `stateAvailable`, `active`;
- `kind` (`DiagnosticsLog` or `SecurityLog`), `requestId`, `fileName`, `attempts`;
- `lastStatus`, `statuses` (statuses delivered for the current or last job, in order);
- `uploads` (completed uploads), `pendingStatuses` and `cancelled` (cancelled `requestId`s).

The event name is `diagnostics_observed`. The snapshot, traces and debug output never contain
locations or log content. The actions are shared with the OCPP 1.6 model; the station's edition
selects the snapshot. See `bins/uob-sim/examples/diagnostics-2.0.1*.toml`.

## Not modeled

- **`ExtendedTriggerMessage`.** It does not exist in OCPP 2.0.1; `TriggerMessage` is modeled.
- **FTP/FTPS, POST uploads and resume.** Uploads use HTTP(S) PUT only (N01.FR.14/18/19/22/23).
- **Security events.** No security log entries are recorded for events (issue #128).

## Verification

```text
cargo test --locked -p uob-sim --test diagnostics201_model --test diagnostics201_wire \
  --test diagnostics201_process --test diagnostics201_scenario --test diagnostics201_independence
```

`diagnostics201_independence` checks that the simulator reaches no bridge crate. It drives the
model with the independently authored corpus requests and negative cases from
`tests/ocpp-fixtures/corpus/wire/2.0.1/log-*.json`, and checks that every emitted status and reply
is in the pinned schema enumerations (all eight `UploadLogStatusEnumType` values are exercised).

The model tests cover both log types, retries, delays, failure statuses, cancel policies,
rejection, strict validation, ordered delivery, triggered statuses and configuration bounds. The
wire tests use a real OCPP-J socket and a loopback HTTP upload receiver:

- both log types' uploads with byte-checked content and exact status sequences;
- an `Idle` trigger without `requestId`, and an `Uploading` trigger with it;
- refused uploads that retry and then fail with the configured status;
- a stalled upload cancelled by a new `GetLog`, with one `AcceptedCanceled` for the old request;
- `NotImplemented` without a model, and `Rejected` with a reason code;
- a status lost with the socket repeated after the station registers again.

The process test kills the real binary mid-upload. It checks that the restarted binary stores the
file once and reports exactly one `Uploaded`, and that no upload location reaches stdout or
stderr.

## Joint smoke

An opt-in proof against the actual `uob` daemon, not part of the workspace verifier:

```text
cargo build -p uob-service -p uob-sim
python3 bins/uob-sim/tests/diagnostics201_joint_smoke.py \
  --bridge target/debug/uob --simulator target/debug/uob-sim --output <fresh dir>
```

Three independent simulator stations connect to one daemon:

- `alpha` uploads a diagnostics log and then a security log, and the bridge-observed sizes match
  the generated files;
- `charlie` has an ongoing upload cancelled by a newer `GetLog` (`AcceptedCanceled`; the old job
  becomes `cancelled`) and the replacement uploads;
- `bravo` has its daemon killed before the upload, so the restarted bridge's in-memory test
  destination is gone and the job honestly reaches `upload_failed`.

The proof also checks that the release-drain inventory empties and that no upload location or
credential appears in results, history or logs.
