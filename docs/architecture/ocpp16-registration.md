# OCPP 1.6 registration and status lifecycle

`v16::complete_registration` consumes a decoded call from the bounded OCPP session receiver.
The authenticated station's ordered task passes its authoritative snapshot, storage port, current
server time, explicit registration decision, and configured nonzero heartbeat/retry interval.
`v16::registration_call` provides the same path for complete wire frames. Authentication alone does
not choose Accepted: the host supplies Accepted, Pending, or Rejected from its registration policy.
The host sends the returned payload through the existing call responder, or rejects that responder
with the returned sanitized CALLERROR.

Boot decisions replace four station points under `ocpp16/registration/`: `status`,
`interval_seconds`, `vendor`, and `model`. They retain transactions, capabilities, and native
connector topology. Heartbeats require accepted registration and an admitted OCPP 1.6 transport;
they update `connectivity.last_message_at` using server receipt time and reply with `currentTime`.
Pending/rejected stations cannot send unsolicited heartbeat/status traffic through this handler.
Boot retry timing belongs to the charger: the response supplies the configured minimum interval;
triggered traffic and configuration while Pending belong to the remote-trigger/configuration work.
The Pending response leaves transport ownership with the existing call session.

Status reports replace five point slots under `ocpp16/connector-N/status/`: `status`, `error_code`,
`info`, `vendor_id`, and `vendor_error_code`. Missing optional error fields explicitly clear older
values. The native status is retained even when the canonical availability projection is coarser:
Available, Unavailable, and Faulted map directly; Preparing, Charging, SuspendedEV/EVSE, Finishing,
and Reserved map to Occupied. This is a reported resource status, not evidence that a command
caused physical charging. Connector zero updates station points and permits only controller
statuses; positive connector IDs must already exist in the authenticated station's topology.
No EVSE identity or new connector is inferred from a report.

Point `source_time` is the optional device timestamp; `observed_at` is the supplied server receipt
time. Without source time, receipt time orders reports. An older effective timestamp is
acknowledged without replacing the current status, while activity still advances. Repeated reports
replace slots rather than growing snapshot history. Exact error information is canonical operational
state; any diagnostic disclosure must still pass through the central redaction boundary.

Every successful transition commits a candidate snapshot before replacing the in-memory state or
returning a success response. Persistence failures return InternalError. After restart, the host
restores the snapshot and sets connectivity from a newly authenticated transport; restored
registration does not itself prove a live connection. This permits reconnect without inventing a
charger reboot. A new BootNotification reapplies current registration policy. All snapshot writes
for a station must run in its existing ordered owner; independent copies are not safe for concurrent
transaction, metering, and registration updates. This feature does not add a second station loop
or enable the still-incomplete daemon-wide charging composition.

## Independent evidence

The unmodified StatusNotification schema comes from the OCA OCPP 1.6 Edition 2 archive whose SHA-256
is pinned in `tests/ocpp-fixtures/corpus/provenance.json`. Its schema and the hand-authored status
wire fixture have individual digest entries in `fixtures.json`. Requirements `ocpp16.registration.boot`,
`ocpp16.heartbeat`, and `ocpp16.status` link to behavioral tests in `coverage.json`.
The relevant specification sections are 4.2 (boot), 4.6 (heartbeat), 4.9 (status), and their message
definitions; this is independent implementation evidence, not certification.

`ocpp16_registration` tests cover all boot outcomes, denied/invalid calls, every native status,
controller scope, optional field clearing, source/receipt timestamps, delayed observations,
SQLite close/reopen recovery, and failed writes without speculative state changes. Its authenticated
WebSocket test delays application handling and verifies that no reply appears before the decision
is durably committed. Run them with:

```text
cargo test --locked -p uob-protocol-adapter --test ocpp16_registration
./scripts/verify-workspace.sh
```
