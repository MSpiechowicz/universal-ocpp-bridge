# Independent OCPP 1.6J firmware station model

`uob-sim` can act as an OCPP 1.6J charge point that receives firmware updates from a CSMS. The model
is derived independently from the pinned OCA sources, not from the bridge implementation:

- OCPP 1.6 Edition 2 §4.5, §5.19, §6.19/6.20, §6.55/6.56 and §7.25, plus errata 3.12 and 3.47–3.49
  (`UpdateFirmware` / `FirmwareStatusNotification`).
- OCPP 1.6 Security Whitepaper Edition 4, use case L01 and §5.19–5.22, §6.8, §6.9 and §6.18
  (`SignedUpdateFirmware` / `SignedFirmwareStatusNotification`).

A station implements exactly one variant. A `legacy` station answers `SignedUpdateFirmware` with
CALLERROR `NotImplemented`. A `signed` station answers `UpdateFirmware` with CALLERROR
`NotSupported` and never starts it (L01.FR.20). Stations without a `firmware16` table keep the
client library's `NotImplemented` reply for both actions.

## Configuration

```toml
[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/ocpp/alpha"
ocpp_version = "1.6"

[stations.firmware16]
private_state_file = "/srv/sim/private/firmware.json"  # required, owner-only like reservation16
mode = "signed"                                        # required: "legacy" or "signed"
manufacturer_root_file = "/srv/sim/manufacturer.pem"   # required for, and only for, "signed"
maximum_bytes = 67108864        # 1..=268435456, default 64 MiB
download_timeout_ms = 30000     # 1..=300000 per attempt
reboot_after_install = true     # reboot, send BootNotification, then report Installed
firmware_version_prefix = ""    # at most 20 printable ASCII characters
wait_for_transactions = true    # do not start installing while a transaction is active
cancel_policy = "cancel"        # "cancel" (L01.FR.26) or "reject" (L01.FR.27)
status_delay_ms = 0             # 0..=30000 before each status CALL
# Fault controls:
download_failures = 0           # 0..=16 initial attempts that fail without network access
fail_install = false            # Installing, then InstallationFailed
fail_install_verification = false  # signed only: InstallVerificationFailed
reject_signed = false           # signed only: reply Rejected
```

The private state file and its `.lock` use the same owner-only, exclusive, fsync-before-rename
storage as the reservation models. The manufacturer root file is public PEM material: an absolute
path to a regular file of at most 32 KiB holding one to four certificates and no private key.
The table is rejected for an OCPP 2.0.1 station.

## Station behavior

Both variants validate the native payload strictly. Unknown fields, a JSON `null` for an optional
field, or a wrong type return CALLERROR `FormationViolation`. A negative or out-of-`int32`
`retries`, `retryInterval` or `requestId`, an invalid RFC 3339 date-time, an empty location, or
signed fields above their schema lengths return `PropertyConstraintViolation`.

A legacy station replies `{}`, waits until `retrieveDate` without reporting, then sends
`Downloading`. It downloads through the bounded `artifact_transfer` client. A failed attempt is
retried up to `retries` times, waiting `retryInterval` seconds (capped at 60) between attempts, and
`Downloading` is sent again for each attempt. When every attempt fails, the station reports
`DownloadFailed`. Otherwise it reports `Downloaded`, waits until no transaction is active, then
reports `Installing`. With a reboot, it disconnects, reconnects, sends `BootNotification` with
`firmwareVersion = <prefix>sha256-<first 12 hex of the image digest>`, and reports `Installed`
once the new socket is accepted. A new `UpdateFirmware` replaces an ongoing legacy job; OCPP 1.6
defines no cancellation.

A signed station:

1. Replies `Rejected` when `reject_signed` is set.
2. Validates the signing certificate before accepting (L01.FR.23). It must be one code-signing
   leaf, issued directly by an installed manufacturer root with no intermediate, and valid now.
   Otherwise the reply is `InvalidCertificate` (L01.FR.24) and nothing starts.
3. Applies `cancel_policy` to an ongoing job: `AcceptedCanceled` (L01.FR.26) or `Rejected`
   (L01.FR.27). Otherwise it replies `Accepted`.
4. Reports `DownloadScheduled` when `retrieveDateTime` is in the future (L01.FR.13). It then runs
   the same download and retry flow, reporting `Downloading` and `Downloaded` or
   `DownloadFailed`.
5. Verifies RSA-PSS SHA-256 over the complete downloaded image using the certificate's key
   (L01.FR.04, L01.FR.12). On success it reports `SignatureVerified`; otherwise `InvalidSignature`.
6. Reports `InstallScheduled` while `installDateTime` is in the future (L01.FR.16). It then
   waits for transactions to end and reports `Installing`.
7. With a reboot, reports `InstallRebooting` before restarting (L01.FR.15, L01.FR.34 option a),
   then `Installed` from the "new firmware" after the accepted `BootNotification`.

Every signed status carries the job's `requestId` (L01.FR.10). A legacy
`TriggerMessage(FirmwareStatusNotification)` returns `Idle` only when no job is active (§4.5).
While a job is active it returns the last reported status. When nothing has been reported yet, the
trigger is answered `Rejected`. A signed station answers that legacy trigger with `Idle` when no
job is active and `Rejected` otherwise; its own trigger is `ExtendedTriggerMessage` (see below).

## Durability and delivery

Each transition is committed to the private state before its status CALL is attempted. Statuses
leave the durable outbox in order, one at a time. They are sent only on a socket whose
`BootNotification` was accepted. A CALLRESULT, a CALLERROR or an undecodable reply counts as
delivered. A timeout or transport failure is retried after one second. Delivery is at least once:
a crash between a CALLRESULT and the following commit can repeat that one status.

After a process restart:

- Waiting phases continue to wait.
- An interrupted download starts that attempt again and reports `Downloading` again.
- A job that was rebooting counts the restart as its reboot and reports `Installed` after the
  next accepted `BootNotification`.

A configured firmware model also makes automatic reconnects re-register before any native call.

## Scenario actions

`assert_firmware` and `await_firmware` compare an `expect_response` subset of the safe snapshot:
`stateAvailable`, `mode`, `active`, `requestId`, `lastStatus`, `statuses` (statuses delivered for
the current or last job, in order), `installedVersion`, `reboots`, `pendingStatuses` and
`cancelled`. The snapshot, traces and debug output never contain locations, certificates,
signatures or digests. See `bins/uob-sim/examples/firmware-1.6*.toml` and
`bins/uob-sim/examples/signed-firmware-1.6*.toml`.

## Not modeled

- **Security events.** `SecurityEventNotification` for `InvalidFirmwareSigningCertificate`,
  `InvalidFirmwareSignature` and `FirmwareUpdated` (L01.FR.02/03/33) is not sent. Security events
  belong to issue #127.
- **Revocation.** `RevokedCertificate` is never returned, because the station holds no revocation
  data.
- **`ExtendedTriggerMessage`.** It is not implemented, so L01.FR.28/29 are not covered.
- **`DownloadPaused`.** It is never reported.
- **L01.FR.07/19.** Unused connectors are not set to `Unavailable` while waiting for transactions
  to end.
- **Legacy install verification.** A legacy station accepts any image whose download completes;
  OCPP 1.6 Edition 2 defines no integrity metadata.

## Verification

```text
cargo test --locked -p uob-sim --test firmware16_model --test firmware16_wire \
  --test firmware16_process --test firmware16_scenario
```

The model tests cover every native sequence, retries, cancel policies, verification and install
failures, durable resume and redaction. The wire tests use a real OCPP-J socket and a local HTTP
artifact server:

- the legacy download, reboot and `Installed` sequence, followed by an `Idle` trigger;
- edition mismatch errors;
- an untrusted certificate, a tampered image, scheduled download, and `AcceptedCanceled`;
- oversized and missing artifacts.

The process test kills the real binary mid-download and checks that the restarted binary
finishes with exactly one `Installed`. It also checks that no private request material reaches
stdout or stderr.
