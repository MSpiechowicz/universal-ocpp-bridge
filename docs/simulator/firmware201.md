# Independent OCPP 2.0.1 firmware station model

`uob-sim` can act as an OCPP 2.0.1 charging station that receives firmware updates from a CSMS. The
model is derived independently from the pinned OCA sources, not from the bridge implementation:

- OCPP 2.0.1 Edition 4 Part 2, use cases L01 (Secure Firmware Update) and L02 (Non-Secure Firmware
  Update), Figure 116 (firmware status transitions).
- Errata 2026-06 §2.14: L01.FR.04 now verifies the request's signature over the entire received
  file with the signature's own hash algorithm (RSA-PSS or ECDSA).
- The byte-exact `UpdateFirmwareRequest`/`Response` and `FirmwareStatusNotificationRequest`/`Response`
  schemas in `tests/ocpp-fixtures/corpus/schemas/2.0.1`.

Unlike OCPP 1.6, one `UpdateFirmware` message carries both use cases. A station implements exactly
one mode. A `secure` station requires the signing certificate and validates it before accepting. A
`non_secure` station has no manufacturer root and ignores any signing material. Stations without a
`firmware201` table keep the client library's CALLERROR `NotImplemented`.

## Configuration

```toml
[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/ocpp/alpha"
ocpp_version = "2.0.1"
reconnect = true                # required while reboot_after_install = true

[stations.firmware201]
private_state_file = "/srv/sim/private/firmware.json"  # required, owner-only like reservation16
mode = "secure"                                        # required: "secure" (L01) or "non_secure" (L02)
manufacturer_root_file = "/srv/sim/manufacturer.pem"   # required for, and only for, "secure"
maximum_bytes = 67108864        # 1..=268435456, default 64 MiB
download_timeout_ms = 30000     # 1..=300000 per attempt
reboot_after_install = true     # InstallRebooting, reconnect, BootNotification, then Installed
firmware_version_prefix = ""    # at most 31 printable ASCII characters (firmwareVersion is CiString50)
wait_for_transactions = true    # do not start installing while a transaction is active
cancel_policy = "cancel"        # "cancel" (L01.FR.24) or "reject" (L01.FR.27)
status_delay_ms = 0             # 0..=30000 before each status CALL
# Fault controls:
download_failures = 0           # 0..=16 initial attempts that fail without network access
fail_install = false            # Installing, then InstallationFailed
fail_install_verification = false  # Installing, then InstallVerificationFailed (L01.FR.29)
reject_updates = false          # reply Rejected to every request
```

The private state file and its `.lock` use the same owner-only, exclusive, fsync-before-rename
storage as the reservation models. The manufacturer root file is public PEM material: an absolute
path to a regular file of at most 32 KiB holding one to four certificates and no private key. The
table is rejected for an OCPP 1.6 station. Because a firmware reboot closes the socket, a station
with `reboot_after_install = true` must set `reconnect = true`, or the client refuses to start.

## Station behavior

The request is first validated against the pinned schema. Unknown fields, a JSON `null`, a missing
`requestId` or `retrieveDateTime`, an over-long location (512), certificate (5 500) or signature
(800), or a date-time with more than millisecond precision return CALLERROR `FormationViolation`.
A negative `retries` or `retryInterval`, or an empty location, certificate or signature, returns
`PropertyConstraintViolation`.

1. With `reject_updates`, the reply is `Rejected`.
2. A secure station validates the signing certificate before accepting (L01.FR.21). It must be
   one code-signing leaf issued directly by an installed manufacturer root, with no intermediate,
   valid now. A missing or untrusted certificate is answered `InvalidCertificate` (L01.FR.22) and
   nothing starts.
3. An ongoing update is cancelled for the new one with `AcceptedCanceled` (L01.FR.24). Statuses of
   the cancelled update that were not sent yet are dropped. With `cancel_policy = "reject"` the
   station keeps its update and answers `Rejected` with `statusInfo.reasonCode = "UnableToCancel"`
   (L01.FR.27). Otherwise the reply is `Accepted`.
4. `DownloadScheduled` is reported while `retrieveDateTime` is in the future (L01.FR.13,
   L02.FR.07). Each attempt reports `Downloading` and downloads through the bounded
   `artifact_transfer` client. A failed attempt is retried up to `retries` times, waiting
   `retryInterval` seconds (capped at 60). When every attempt failed, the station reports
   `DownloadFailed` (L01.FR.30). Otherwise it reports `Downloaded`.
5. A secure station verifies the signature over the complete image with the certificate's key:
   RSA-PSS with SHA-256, or ECDSA P-256/SHA-256 or P-384/SHA-384 (errata L01.FR.04, L01.FR.12). A
   missing or failing signature reports `InvalidSignature`; success reports `SignatureVerified`.
   A non-secure station skips this step.
6. `InstallScheduled` is reported while `installDateTime` is in the future (L01.FR.16,
   L02.FR.10). Installation then waits for active transactions to end (L01.FR.06) and reports
   `Installing`, followed by `InstallVerificationFailed` or `InstallationFailed` when configured.
7. With a reboot, the station reports `InstallRebooting` (L01.FR.15, L01.FR.32 option a). Once
   that status is delivered, it closes the socket and reconnects. Its `BootNotification` then
   carries `reason = "FirmwareUpdate"` and
   `chargingStation.firmwareVersion = <prefix>sha256-<first 12 hex of the image digest>`. It
   reports `Installed` once that Boot is accepted. Without a reboot, `Installed` follows
   `Installing` directly.

Every status carries the requestId of the request that started the update (L01.FR.10/20). A
`TriggerMessage(FirmwareStatusNotification)` returns `Idle` when nothing has been sent yet or the
last sent status was `Installed` (L01.FR.25). Otherwise it returns the last sent status with its
requestId (L01.FR.26).

## Durability and delivery

Each transition is committed to the private state before its status CALL is attempted. Statuses
leave the durable outbox in order, one at a time, and only on a socket whose `BootNotification`
was accepted. A CALLRESULT, a CALLERROR or an undecodable reply counts as delivered. A timeout or
transport failure is retried after one second. Delivery is at least once: a crash between a
CALLRESULT and the following commit can repeat that one status.

After a process restart:

- Waiting phases continue to wait.
- An interrupted download starts that attempt again and reports `Downloading` again.
- A job that was rebooting counts the restart as its reboot and reports `Installed` after the next
  accepted `BootNotification`.

A configured firmware model makes every automatic reconnect send `BootNotification` before any
native call, even without persistent local-authorization state.

## Scenario actions

`assert_firmware` and `await_firmware` are shared with the 1.6 model and compare an
`expect_response` subset of the safe snapshot: `stateAvailable`, `mode`, `active`, `requestId`,
`lastStatus`, `statuses` (delivered for the current or last update, in order), `installedVersion`,
`reboots`, `pendingStatuses` and `cancelled` (requestIds). The snapshot, traces and debug output
never contain locations, certificates, signatures or digests. See
`bins/uob-sim/examples/firmware-2.0.1*.toml` and
`bins/uob-sim/examples/non-secure-firmware-2.0.1-config.toml`.

## Not modeled

- **Security events.** `SecurityEventNotification` for `InvalidFirmwareSigningCertificate`,
  `InvalidFirmwareSignature` and `FirmwareUpdated` (L01.FR.02/03/31) is not sent; it belongs to the
  2.0.1 security-event work (#128).
- **Revocation.** `RevokedCertificate` is never returned; the station holds no revocation data.
- **`DownloadPaused`.** It is never reported.
- **L01.FR.07/L02.FR.04.** Unused EVSEs are not set to `Unavailable` while waiting for transactions.
- **Optional failure reports for a cancelled update** (L01.FR.24 note) are not sent.
- **L03/L04.** Publishing firmware on a Local Controller.

## Verification

```text
cargo test --locked -p uob-sim --test firmware201_model --test firmware201_wire \
  --test firmware201_process --test firmware201_scenario --test firmware201_independence
cargo test --locked -p uob-ocpp-fixtures --test firmware201
```

`firmware201_independence` drives the model with the hand-authored corpus fixtures in
`tests/ocpp-fixtures/corpus/wire/2.0.1`. Corpus requests produce the corpus replies, and no
negative case starts a native process. The model tests cover the secure and non-secure sequences,
certificate and signature refusal (RSA-PSS and ECDSA), schedules, retries, both cancel policies,
install failures, transaction waits, trigger replies and ordered at-least-once delivery. The wire
tests use a real OCPP-J socket and a local HTTP artifact server:

- an untrusted certificate, a secure download, the reboot with its `FirmwareUpdate` Boot and
  `Installed`;
- an `Idle` trigger after `Installed` and a last-status trigger with its requestId;
- `AcceptedCanceled` and a `FormationViolation` for an unknown field;
- retried missing downloads, a non-secure install, and `NotImplemented` without a model.

The process test kills the real binary mid-download and checks that the restarted binary finishes
with exactly one `Installed` and leaks no private request material to stdout or stderr. The opt-in
`firmware201_joint_smoke.py` runs two such stations (secure and non-secure) against a real `uob`
daemon, including a daemon restart mid-download:

```text
cargo build -p uob-service -p uob-sim
python3 bins/uob-sim/tests/firmware201_joint_smoke.py --bridge target/debug/uob \
  --simulator target/debug/uob-sim --output <fresh private directory>
```
