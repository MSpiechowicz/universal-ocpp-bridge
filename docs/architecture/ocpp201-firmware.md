# OCPP 2.0.1 firmware update orchestration

The bridge sends OCPP 2.0.1 `UpdateFirmware` (Part 2 use cases L01 Secure Firmware Update and
L02 Non-Secure Firmware Update, errata 2026-06) and tracks each request as a durable job until
the station reports an end state. Unlike OCPP 1.6, one message carries both security modes. The
mode is station policy, not a caller choice:

- A **secure** station (the default) receives the signing certificate and the signature with
  every request (L01.FR.11). Only a `SignedFirmware` artifact can be sent to it.
- A **non-secure** station (`non_secure_firmware = true`) receives neither. Only an unsigned
  `Firmware` artifact can be sent to it.

## Request path

1. A privileged caller names a provider artifact; it never supplies a station location,
   certificate or signature. The payload schema is
   `urn:uob:ocpp201:UpdateFirmwareReference:1` with `requestId`, `artifactReference`,
   `retrieveDateTime` and optional `installDateTime`, `retries` and `retryInterval`. It
   addresses only the station root (no EVSE).
2. Before durable admission, `RemoteControlSession::firmware_context` checks the privileged
   grant, an accepted registration, the advertised capability and the live socket. It derives
   the job deadline: the later of the retrieval time, the installation time and now, plus the
   station's `firmware_job_timeout_seconds`.
3. The command, its admitted result, the `firmware201_jobs` row and the `release_jobs` entry
   commit in one SQLite transaction. The application refuses a firmware action whose port
   produced no job, so nothing is sent without registration. The store refuses a second
   request in flight for the same station, a `requestId` already retained for the station
   (reports are matched by it alone), and a full drain inventory (128 entries).
4. At dispatch the session resolves the artifact through the `ArtifactProvider` (10 s bound).
   It refuses test material where the runtime policy forbids it and requires the artifact kind
   to match the station's mode. For a secure station it verifies the signing certificate with
   the `CertificateProvider` under `ChainPurpose::FirmwareSigning`, so a trusted station never
   has to answer `InvalidCertificate` (L01.FR.21/22). Any refusal is `not_sent` with a
   sanitized detail and no bytes on the wire. Authority is checked again under the snapshot
   lock after the provider calls, immediately before the call is queued.
5. The native reply is kept exactly: one of the five `UpdateFirmwareStatusEnumType` values
   with its optional `statusInfo.reasonCode`, or the sanitized 2.0.1 CALLERROR code.
   `statusInfo.additionalInfo` is free station text and is validated but not retained. A
   malformed reply, timeout or lost socket is `transmission_uncertain` and is never replayed
   after restart or reconnect.

## Job lifecycle

The states, and which of them release the drain, are the same as for
[OCPP 1.6](ocpp16-firmware.md#job-lifecycle). `InvalidCertificate`, `RevokedCertificate`,
`Rejected` and a CALLERROR all end the job as `rejected`.

Native notifications commit before their empty `FirmwareStatusNotificationResponse`. Every
report for an update carries that update's `requestId` (L01.FR.10), and the decoder refuses a
report without one unless its status is `Idle` (L01.FR.20). A report advances only the job
with its exact `requestId`; a report for an unknown `requestId` changes nothing but is still
acknowledged. An identity-free `Idle` means the station has no firmware work in progress. It
settles the newest unresolved job the station has already answered as `station_idle`; a job
still `pending` or `uncertain` is left alone, because the station may not have seen it yet.

Progress follows the phases of Figure 116:

1. `DownloadScheduled`
2. `Downloading`/`DownloadPaused`
3. `Downloaded`
4. `SignatureVerified`
5. `InstallScheduled`
6. `InstallRebooting`/`Installing`

Statuses in the same phase may alternate, which covers retries (L01.FR.30), pauses
(L01.FR.14) and the reboots of L01.FR.15/L01.FR.32. A regression, a failure end state that
contradicts reported progress (for example `DownloadFailed` after `Downloaded`), or a
signature status for a non-secure update is counted in `rejected_transitions` and ignored.
Facts for a resolved job are counted but never revive it.

An accepted request supersedes every older unresolved job of the station. `AcceptedCanceled`
(L01.FR.24) marks the older job `cancelled`; a plain `Accepted` marks it `superseded`. The
station may still report `DownloadFailed` or `InstallationFailed` for the cancelled update;
that report is counted but does not change it. Startup recovery marks a job `uncertain` if
dispatch had started and `not_sent` otherwise, and restores a missing release-drain entry.
After an `Installed` report, a triggered `FirmwareStatusNotification` returns `Idle`
(L01.FR.25); otherwise it repeats the last status with its `requestId` (L01.FR.26). Both
reconcile normally and also count as trigger evidence.

## Public evidence

`CommandResult.firmware_201` (contract revision 15) carries:

- the native `request_id` and whether the update was `secure`;
- the sent artifact's reference, SHA-256, size and `signed`/`test_only` flags;
- the native reply;
- the durable job, refreshed in the stored result whenever the job changes.

Locations, certificates, signatures, `additionalInfo` and raw payloads never appear in
results, history or exports.

## Not covered

- `SecurityEventNotification` for `InvalidFirmwareSigningCertificate`,
  `InvalidFirmwareSignature` and `FirmwareUpdated` (L01.FR.02/03/31) belongs to the 2.0.1
  security-event work (#128).
- L03/L04 publishing firmware on a Local Controller. The bridge is a CSMS, not a Local
  Controller.
- Station-side behavior the CSMS cannot enforce: waiting for transactions (L01.FR.06/33),
  setting EVSEs unavailable (L01.FR.07), and the signature check itself (L01.FR.04/12). The
  independent simulator models these.
- Certificate revocation checking, persistent production artifact storage, production PKI,
  physical stations and OCA certification.

## Verification

```text
cargo test --locked -p uob-contracts --test firmware201
cargo test --locked -p uob-application --lib firmware201
cargo test --locked -p uob-storage-adapter --test firmware201
cargo test --locked -p uob-protocol-adapter --lib firmware
cargo test --locked -p uob-protocol-adapter --test command_registry --test ocpp201_firmware_decode
cargo test --locked -p uob-service --test firmware201
cargo test --locked -p uob-sim --test firmware201_model --test firmware201_wire --test firmware201_process --test firmware201_scenario --test firmware201_independence
cargo test --locked -p uob-ocpp-fixtures --test firmware201
cargo build -p uob-service -p uob-sim
python3 bins/uob-sim/tests/firmware201_joint_smoke.py --bridge target/debug/uob \
  --simulator target/debug/uob-sim --output <fresh private directory>
```

The joint smoke is opt-in and is not part of `scripts/verify-workspace.sh`. It starts a real
daemon with the demo artifact service and two independent 2.0.1 simulator stations, one secure
and one non-secure. The secure station verifies the bridge's signature against the exported
manufacturer root. The daemon is killed during the non-secure download and restarted. The
smoke then checks:

- both jobs reach `installed` with the exact native status sequences and `requestId`s;
- the release-drain inventory empties;
- results and logs contain no station locations, certificates or credentials.
