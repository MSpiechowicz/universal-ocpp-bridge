# OCPP 2.0.1 log retrieval

The bridge sends OCPP 2.0.1 `GetLog` (N01, Part 2 Edition 4 §1.28) for `DiagnosticsLog` or
`SecurityLog` and tracks each request as a durable upload job until the station reports an end
state. A station's `Uploaded` claim is never taken on its word: the bridge asks the artifact
provider what it actually stored for that job's destination. OCPP 2.0.1 has no
`GetDiagnostics`; both log types use the one message. This is the counterpart of the Security
Whitepaper `GetLog` in [OCPP 1.6J diagnostics](ocpp16-diagnostics.md), with the 2.0.1
message, status and numbering.

## Request path

1. A privileged caller sends a request without any upload location. The payload schema is
   `urn:uob:ocpp201:GetLogReference:1`: `logType`, the native signed int32 `requestId`,
   `oldestTimestamp?`, `latestTimestamp?`, `retries?` and `retryInterval?`. It addresses only the
   station root. A window that ends before it starts is refused by the contract decoder.
   `log.remoteLocation` is never accepted from a caller.
2. Before durable admission, `RemoteControlSession::diagnostics_context` checks the privileged
   grant, an accepted registration, the advertised `GetLog` capability (enabled per station with
   `get_log`) and the live socket. The job deadline is admission time plus the station's
   `diagnostics_job_timeout_seconds`.
3. The command, its admitted result, the `diagnostics201_jobs` row and its
   `diagnostics201/<sha256>` `release_jobs` entry (kind `diagnostics`) commit in one SQLite
   transaction (schema v21). The application refuses a 2.0.1 `GetLog` whose port produced no
   job. The store refuses:
   - a second in-flight request for the same station;
   - a reused `requestId`, because reports are matched by it alone (N01.FR.07);
   - a full job history (64 revisions per station) or a full drain inventory.

   A `GetLog` may be admitted while another upload is in progress: the station then cancels the
   earlier one and answers `AcceptedCanceled` (N01.FR.12).
4. At dispatch the session opens one bounded `UploadDestination` through the `ArtifactProvider`
   (10 s bound). It refuses test destinations where the runtime policy forbids them, a kind that
   does not match the log type, and a cap above the station's `diagnostics_upload_max_bytes`.
   The destination identity is then durably bound to the job. Only after that binding commits is
   the native request sent, so every later `Uploaded` can be checked. The send itself rechecks
   authority under the snapshot lock. Any refusal is `not_sent` with a sanitized detail and no
   bytes on the wire.
5. The native reply is kept exactly: the `GetLogResponse` status, its optional `filename`
   (printable ASCII, at most 255 characters) and `statusInfo.reasonCode` (1 to 20 printable
   ASCII characters; `additionalInfo` is validated, not retained), or the sanitized CALLERROR
   code. `Accepted` and `AcceptedCanceled` mean the station took the upload, `Rejected` is a
   protocol rejection (N01.FR.05) and nothing will be uploaded. A malformed reply, a timeout or
   a lost socket is `transmission_uncertain` and is never replayed after restart or reconnect.

## Job lifecycle

| State | Meaning | Releases the drain |
|---|---|---|
| `pending` | Admitted; no correlated reply yet | No |
| `uncertain` | Possibly delivered without a reply | No |
| `accepted` | Station answered `Accepted` or `AcceptedCanceled` | No |
| `uploading` | The station reported `Uploading` | No |
| `timed_out` | Deadline passed without an end state | No |
| `uploaded` | Station reported `Uploaded` and the provider holds a complete file for this job | Yes |
| `upload_unconfirmed` | Station reported `Uploaded`, but the provider holds no complete file for this job | Yes |
| `upload_failed`, `bad_message`, `not_supported_operation`, `permission_denied` | Native failure end states (`UploadFailure` maps to `upload_failed`) | Yes |
| `rejected`, `not_sent` | Refused by the station or never transmitted | Yes |
| `cancelled`, `superseded` | Ended by a newer accepted request, or by the station's own `AcceptedCanceled` report | Yes |
| `station_idle` | Station reports no upload in progress; outcome not reported | Yes |

`LogStatusNotification` commits before its empty CALLRESULT and advances only the job with its
exact `requestId` (N01.FR.07). An unmatched `requestId` changes no job but is still
acknowledged. The decoder accepts the nine native shapes: the eight `UploadLogStatusEnumType`
values (`BadMessage`, `Idle`, `NotSupportedOperation`, `PermissionDenied`, `Uploaded`,
`UploadFailure`, `Uploading`, `AcceptedCanceled`) with a `requestId`, and `Idle` without one.
Every other status without a `requestId` is refused with `PropertyConstraintViolation`, and the
1.6-only `UploadFailed` is not a 2.0.1 value (N01.FR.13).

An identity-free `Idle` is accepted only as the answer to a pending `TriggerMessage` for
`LogStatusNotification` (N01.FR.13: the field is mandatory unless the message was triggered
and no upload is ongoing). Without a pending trigger it is refused with
`OccurrenceConstraintViolation` and changes nothing. When accepted it settles only a job the
station has already answered (`accepted`, `uploading` or `timed_out`) as `station_idle`; an
unanswered `pending` or `uncertain` job is left alone. The same notification is also trigger
evidence, committed in the same write.

`AcceptedCanceled` in a notification (N01.FR.20) means the station cancelled the upload named
by its `requestId`, and the job becomes `cancelled`. The `GetLogResponse` `AcceptedCanceled` of
the newer request marks every older unresolved job `cancelled` (N01.FR.12); a plain `Accepted`
marks it `superseded`. Whichever arrives first settles the job; the other is counted as a late
fact. Facts for a resolved job are counted in `notifications` but never revive it.

For `Uploaded`, the service attributes the notification to its job, queries the provider's
`UploadStatus` for that job's bound destination (5 s bound), and commits the SHA-256 and size the
provider computed over exactly the bytes it stored. A provider fact is accepted only for the
job's own destination. Startup recovery marks a job `uncertain` if dispatch had started and
`not_sent` otherwise, and restores a missing release-drain entry. A deadline marks a job
`timed_out` without releasing the drain.

## Public evidence

`CommandResult.diagnostics_201` (contract revision 17) carries:

- the `log_type` and native `request_id`;
- the offered destination's `log_type`, effective `maximum_bytes` and `test_only` flag;
- the exact native reply (`status` with optional `file_name` and `reason_code`, or `call_error`);
- the durable job, refreshed in the stored result whenever the job changes, including the
  provider-observed `upload` facts for an `uploaded` job.

Upload locations, destination identities, log contents and raw payloads never appear in results,
history or exports.

## Demo composition

Uploads go to the same loopback `TestArtifactService` as firmware and as the 1.6 workflows.
`[charging.firmware]` serves all of them; its `catalog_file` is required exactly when a station
enables a firmware update. Each log-enabled station allows four open destinations, at most 64 in
total; the oldest idle one is evicted. The service keeps destinations in memory, so an upload
interrupted by a daemon restart fails and the station reports the failure.

## Not covered

- `ExtendedTriggerMessage` (a 1.6 Whitepaper message with no 2.0.1 counterpart; 2.0.1 uses
  `TriggerMessage`, which is covered).
- `SecurityEventNotification` and the security-log contents themselves (#128).
- FTP/FTPS uploads, resume, `Expect: 100-continue` and basic authorization in upload locations
  (N01.FR.14, N01.FR.17, N01.FR.22, N01.FR.23).
- Monitoring and event reporting (#126), persistent production artifact storage, physical
  stations and OCA certification.

## Verification

```text
cargo test --locked -p uob-contracts --test diagnostics201
cargo test --locked -p uob-application --lib diagnostics201
cargo test --locked -p uob-storage-adapter --test diagnostics201
cargo test --locked -p uob-protocol-adapter --test command_registry --test ocpp201_diagnostics_decode --test ocpp201_trigger
cargo test --locked -p uob-protocol-adapter --lib diagnostics
cargo test --locked -p uob-service --test diagnostics201
cargo test --locked -p uob-sim --test diagnostics201_model --test diagnostics201_wire \
  --test diagnostics201_process --test diagnostics201_scenario --test diagnostics201_independence
cargo test --locked -p uob-ocpp-fixtures --test diagnostics201
cargo build -p uob-service -p uob-sim
python3 bins/uob-sim/tests/diagnostics201_joint_smoke.py --bridge target/debug/uob \
  --simulator target/debug/uob-sim --output <fresh private directory>
```

The joint smoke is opt-in and is not part of `scripts/verify-workspace.sh`. It starts a real
daemon with the demo artifact service and independent 2.0.1 simulator stations. One uploads a
diagnostics log and then a security log, and the bridge's provider-observed sizes must match the
files the station generated. A second station exercises `AcceptedCanceled`. The daemon is killed
before a third station's upload and is then restarted. The smoke then checks:

- the interrupted job reaches `upload_failed`;
- the release-drain inventory empties;
- results and logs contain no upload locations or credentials.
