# OCPP 1.6J firmware update orchestration

The bridge sends OCPP 1.6 `UpdateFirmware` (Edition 2 §5.19, §6.55) or the Security
Whitepaper Edition 4 `SignedUpdateFirmware` (L01, §5.21). It then tracks each request as a
durable job until the station reports an end state. A station supports exactly one of the
two message families: a signed-only station answers the original message with CALLERROR
`NotSupported` and does not start (L01.FR.20). The bridge therefore offers one family per
station and never falls back to the other.

## Request path

1. A privileged caller names a provider artifact; it never supplies a station location,
   certificate or signature. The payload schemas are
   `urn:uob:ocpp16:UpdateFirmwareReference:1` and
   `urn:uob:ocpp16:SignedUpdateFirmwareReference:1`. Both address only the station root.
2. Before durable admission, `RemoteControlSession::firmware_expectation_16` checks the
   privileged grant, the configured family, an accepted registration, the advertised
   capability and the live socket. It derives the job deadline: the later of the retrieval
   time, the installation time and now, plus the station's `firmware_job_timeout_seconds`.
3. The command, its admitted result, the `firmware16_jobs` row and the `release_jobs` entry
   commit in one SQLite transaction. The application refuses a firmware action whose port
   produced no job, so nothing is sent without registration. The store refuses a second
   in-flight request for the same station, a legacy request while another legacy job is
   still progressing, a reused signed `requestId`, and a full drain inventory (128 entries).
4. At dispatch the session resolves the artifact through the `ArtifactProvider` (10 s
   bound). It refuses test material where the runtime policy forbids it and requires the
   artifact kind to match the family. For a signed artifact it verifies the signing
   certificate with the `CertificateProvider` under `ChainPurpose::FirmwareSigning`
   (A00.FR.706). Any refusal is `not_sent` with a sanitized detail and no bytes on the wire.
5. The native reply is kept exactly. It is the empty legacy acknowledgement, one of the five
   signed statuses, or the sanitized CALLERROR code. A malformed reply, timeout or lost
   socket is `transmission_uncertain` and is never replayed after restart or reconnect.

## Job lifecycle

| State | Meaning | Releases the drain |
|---|---|---|
| `pending` | Admitted; no correlated reply yet | No |
| `uncertain` | Possibly delivered without a reply | No |
| `accepted` | Acknowledged or `Accepted`/`AcceptedCanceled` | No |
| `download_scheduled` … `installing` | Last native progress status | No |
| `timed_out` | Deadline passed without an end state | No |
| `installed` | Native success end state | Yes |
| `download_failed`, `installation_failed`, `install_verification_failed`, `invalid_signature` | Native failure end states | Yes |
| `rejected`, `not_sent` | Refused by the station or never transmitted | Yes |
| `cancelled`, `superseded` | Ended by a newer accepted request | Yes |
| `station_idle` | Station reports no firmware work; success not reported | Yes |

Native notifications commit before their empty CALLRESULT. A legacy
`FirmwareStatusNotification` has no identity and belongs to the newest unresolved legacy job.
After an unconfirmed job is replaced, a late report is therefore attributed to the newer job;
this is an inherent OCPP 1.6 limitation. A `SignedFirmwareStatusNotification` advances only
the job with its exact `requestId` (L01.FR.10). It may omit the identity only for `Idle`
(L01.FR.21); the decoder answers anything else with `PropertyConstraintViolation`.

Progress is phase-monotonic:

1. `DownloadScheduled`
2. `Downloading`/`DownloadPaused`
3. `Downloaded`
4. `SignatureVerified`
5. `InstallScheduled`
6. `InstallRebooting`/`Installing`

Statuses in the same phase may alternate, which covers retries, pauses and the
L01.FR.15/L01.FR.34 reboot. A regression, a failure end state that contradicts reported
progress (for example `DownloadFailed` after `Downloaded`), or a signed-only status on a
legacy job is counted in `rejected_transitions` and ignored. Facts for a resolved job are
counted but never revive it.

An accepted request supersedes every older unresolved job of the station: the station
either cancelled it (`AcceptedCanceled`, L01.FR.26) or no longer had it in progress.
Startup recovery marks a job `uncertain` if dispatch had started and `not_sent` otherwise,
and restores a missing release-drain entry. `Idle` reported after a `TriggerMessage`
resolves a stalled job as `station_idle`. The same notification still counts as trigger
evidence.

## Public evidence

`CommandResult.firmware_16` (contract revision 14) carries:

- the action, and for signed requests the `request_id`;
- the sent artifact's reference, SHA-256, size and `signed`/`test_only` flags;
- the native reply;
- the durable job, refreshed in the stored result whenever the job changes.

Locations, certificates, signatures and raw payloads never appear in results, history or
exports.

## Not covered

- `SecurityEventNotification` for `InvalidFirmwareSigningCertificate`,
  `InvalidFirmwareSignature` and `FirmwareUpdated` (L01.FR.02/03/33) belongs to the 1.6
  security-event work (#127).
- `ExtendedTriggerMessage` (L01.FR.28/29) and certificate revocation checking.
- Persistent production artifact storage and production PKI.
- Physical stations and OCA certification.

## Verification

```text
cargo test --locked -p uob-contracts --test firmware16
cargo test --locked -p uob-application --lib firmware16
cargo test --locked -p uob-storage-adapter --test firmware16
cargo test --locked -p uob-protocol-adapter --test command_registry --test ocpp16_firmware_decode
cargo test --locked -p uob-service --test firmware16
cargo test --locked -p uob-sim --test firmware16_model --test firmware16_wire --test firmware16_process --test firmware16_independence
cargo test --locked -p uob-ocpp-fixtures --test firmware16
cargo build -p uob-service -p uob-sim
python3 bins/uob-sim/tests/firmware16_joint_smoke.py --bridge target/debug/uob \
  --simulator target/debug/uob-sim --output <fresh private directory>
```

The joint smoke is opt-in and is not part of `scripts/verify-workspace.sh`. It starts a real
daemon with the demo artifact service and two independent simulator stations, one legacy
and one signed. The signed station verifies the bridge's signature against the exported
manufacturer root. The daemon is killed during the legacy download and restarted. The smoke
then checks:

- both jobs reach `installed` with the exact native status sequences;
- the release-drain inventory empties;
- results and logs contain no station locations, certificates or credentials.
