# OCPP 1.6J diagnostics and log retrieval

The bridge sends OCPP 1.6 `GetDiagnostics` (Edition 2 §5.9, §6.25) and the Security Whitepaper
Edition 4 `GetLog` (N01, §5.9) for `DiagnosticsLog` or `SecurityLog`. It then tracks each request
as a durable upload job until the station reports an end state. A station's `Uploaded` claim is
never taken on its word: the bridge asks the artifact provider what it actually stored for that
job's destination.

## Request path

1. A privileged caller sends a request without any upload location. The payload schemas are
   `urn:uob:ocpp16:GetDiagnosticsReference:1` (`startTime?`, `stopTime?`, `retries?`,
   `retryInterval?`) and `urn:uob:ocpp16:GetLogReference:1` (`logType`, `requestId`,
   `oldestTimestamp?`, `latestTimestamp?`, `retries?`, `retryInterval?`). Both address only the
   station root. A window that ends before it starts is refused by the contract decoder.
2. Before durable admission, `RemoteControlSession::diagnostics_context` checks the privileged
   grant, the enabled family (`get_diagnostics` and `get_log` are independent), an accepted
   registration, the advertised capability and the live socket. The job deadline is admission
   time plus the station's `diagnostics_job_timeout_seconds`.
3. The command, its admitted result, the `diagnostics16_jobs` row and its
   `diagnostics16/<sha256>` `release_jobs` entry commit in one SQLite transaction (schema v20).
   The application refuses a `GetDiagnostics` or 1.6 `GetLog` whose port produced no job. The
   store refuses:
   - a second in-flight request for the same station;
   - any request while a legacy upload is still expected (OCPP 1.6 defines no cancellation);
   - a reused `GetLog` `requestId`, because reports are matched by it alone;
   - a full drain inventory (128 entries).

   A `GetLog` may be admitted while another log upload is in progress (N01.FR.11).
4. At dispatch the session opens one bounded `UploadDestination` through the `ArtifactProvider`
   (10 s bound). It refuses test destinations where the runtime policy forbids them, a kind that
   does not match the log type, and a cap above the station's `diagnostics_upload_max_bytes`.
   The destination identity is then durably bound to the job. Only after that binding commits is
   the native request sent, so every later `Uploaded` can be checked. Any refusal is `not_sent`
   with a sanitized detail and no bytes on the wire.
5. The native reply is kept exactly: the optional `GetDiagnostics.conf` `fileName`, the
   `GetLog.conf` status with its optional `filename`, or the sanitized CALLERROR code. A file name
   must be printable ASCII of at most 255 characters. A malformed reply, a timeout or a lost
   socket is `transmission_uncertain` and is never replayed after restart or reconnect.

## Job lifecycle

| State | Meaning | Releases the drain |
|---|---|---|
| `pending` | Admitted; no correlated reply yet | No |
| `uncertain` | Possibly delivered without a reply | No |
| `accepted` | A file name was returned, or `Accepted`/`AcceptedCanceled` | No |
| `uploading` | The station reported `Uploading` | No |
| `timed_out` | Deadline passed without an end state | No |
| `uploaded` | Station reported `Uploaded` and the provider holds a complete file for this job | Yes |
| `upload_unconfirmed` | Station reported `Uploaded`, but the provider holds no complete file for this job | Yes |
| `upload_failed`, `bad_message`, `not_supported_operation`, `permission_denied` | Native failure end states (`UploadFailed`/`UploadFailure` map to `upload_failed`) | Yes |
| `no_log_available` | `GetDiagnostics.conf` without a file name (§5.9) | Yes |
| `rejected`, `not_sent` | Refused by the station or never transmitted | Yes |
| `cancelled`, `superseded` | Ended by a newer accepted request | Yes |
| `station_idle` | Station reports no upload in progress; outcome not reported | Yes |

Native notifications commit before their empty CALLRESULT. A `DiagnosticsStatusNotification` has
no identity and belongs to the newest unresolved `GetDiagnostics` job. A `LogStatusNotification`
advances only the job with its exact `requestId` (N01.FR.07). It may omit the identity only for
`Idle` (N01.FR.12); the decoder answers anything else with `PropertyConstraintViolation`. An
identity-free `Idle` settles only a log job the station has already answered. The decoder also
refuses `UploadFailed` in a log notification. `AcceptedCanceled` is named by N01.FR.20 but is not
in the pinned `UploadLogStatusEnumType`, so it is refused as schema-invalid. A status from the
other family is counted in `rejected_transitions` and ignored. Facts for a resolved job are counted
but never revive it.

For `Uploaded`, the service attributes the notification to its job, queries the provider's
`UploadStatus` for that job's bound destination (5 s bound), and commits the SHA-256 and size the
provider computed over exactly the bytes it stored. A provider fact is accepted only for the
job's own destination.

An accepted request supersedes every older unresolved job of the station. `AcceptedCanceled`
marks the older one `cancelled` (N01.FR.11), a plain acceptance marks it `superseded`. Startup
recovery marks a job `uncertain` if dispatch had started and `not_sent` otherwise, and restores a
missing release-drain entry. A triggered `Idle` resolves a stalled job as `station_idle`; the same
notification still counts as trigger evidence.

## Public evidence

`CommandResult.diagnostics_16` (contract revision 16) carries:

- the action and, for `GetLog`, the `log_type` and native `request_id`;
- the offered destination's `log_type`, effective `maximum_bytes` and `test_only` flag;
- the exact native reply;
- the durable job, refreshed in the stored result whenever the job changes, including the
  provider-observed `upload` facts for an `uploaded` job.

Upload locations, destination identities, log contents and raw payloads never appear in results,
history or exports.

## Demo composition

Uploads go to the same loopback `TestArtifactService` as firmware. `[charging.firmware]` serves
both; its `catalog_file` is required exactly when a station enables a firmware update. Each
diagnostics-enabled station allows four open destinations, at most 64 in total; the oldest idle
one is evicted. The service keeps destinations in memory, so an upload interrupted by a daemon
restart fails and the station reports the failure.

## Not covered

- `ExtendedTriggerMessage` for `LogStatusNotification` (the N01.FR.12 trigger path).
- `SecurityEventNotification` and the security-log contents themselves (N01.FR.05, #127).
- FTP/FTPS uploads, resume, `Expect: 100-continue` and basic authorization in upload locations
  (N01.FR.14, N01.FR.17, N01.FR.22, N01.FR.23).
- OCPP 2.0.1 `GetLog` (#125), persistent production artifact storage, physical stations and OCA
  certification.

## Verification

```text
cargo test --locked -p uob-contracts --test diagnostics16
cargo test --locked -p uob-application --lib diagnostics16
cargo test --locked -p uob-storage-adapter --test diagnostics16
cargo test --locked -p uob-protocol-adapter --test command_registry --test ocpp16_diagnostics_decode
cargo test --locked -p uob-protocol-adapter --lib diagnostics
cargo test --locked -p uob-service --test diagnostics16
cargo test --locked -p uob-sim --test diagnostics16_model --test diagnostics16_wire \
  --test diagnostics16_process --test diagnostics16_scenario --test diagnostics16_independence
cargo test --locked -p uob-ocpp-fixtures --test diagnostics16
cargo build -p uob-service -p uob-sim
python3 bins/uob-sim/tests/diagnostics16_joint_smoke.py --bridge target/debug/uob \
  --simulator target/debug/uob-sim --output <fresh private directory>
```

The joint smoke is opt-in and is not part of `scripts/verify-workspace.sh`. It starts a real
daemon with the demo artifact service and two independent simulator stations. One uploads a
diagnostics file and then a security log, and the bridge's provider-observed sizes must match the
files the station generated. The daemon is killed before the other station uploads and is then
restarted. The smoke then checks:

- that job reaches `upload_failed`;
- the release-drain inventory empties;
- results and logs contain no upload locations or credentials.
