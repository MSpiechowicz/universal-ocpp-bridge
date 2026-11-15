# Independent OCPP 1.6J security event and extended trigger station model

`uob-sim` can act as an OCPP 1.6J charge point that reports security events and answers
`ExtendedTriggerMessage`. The model is derived independently from the pinned OCA sources, not from
the bridge implementation:

- OCPP 1.6 Security Whitepaper Edition 4, use case A04 (`SecurityEventNotification`), N01.FR.12,
  L01.FR.28/29, §5.5/§5.6 (`ExtendedTriggerMessage`) and the section 8 event names.
- The pinned schemas under `tests/ocpp-fixtures/corpus/schemas/1.6-security/`.

Stations without a `security16` table keep the client library's `NotImplemented` reply for
`ExtendedTriggerMessage` and never send a security event.

## Configuration

```toml
[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/ocpp/alpha"
ocpp_version = "1.6"
reconnect = true

[stations.security16]
private_state_file = "/srv/sim/private/security.json"  # required, owner-only like diagnostics16
# Fault knobs (both default to false):
duplicate_acknowledged = false  # send every acknowledged event once more, identically
send_before_accepted = false    # deliver before the CSMS accepted this connection's Boot

[stations.security16.extended_trigger]
# "accepted", "rejected" or "not_implemented" per requestedMessage (default "accepted"):
boot_notification = "accepted"
log_status_notification = "accepted"
firmware_status_notification = "accepted"
heartbeat = "accepted"
meter_values = "accepted"
status_notification = "accepted"
sign_charge_point_certificate = "not_implemented"  # default; certificates are not modelled
reply_delay_ms = 0              # 0..=30000 before the ExtendedTriggerMessage.conf is written
omit = false                    # answer Accepted, but never send the triggered message
```

The private state file and its `.lock` use the same owner-only, exclusive, fsync-before-rename
storage as the other native models. The table is rejected for an OCPP 2.0.1 station. A reconnecting
station re-registers with its last `BootNotification` before it sends anything else.

## Security events and guaranteed delivery

A04.FR.02 requires queued, guaranteed delivery. The simulator keeps a durable outbox in the private
state file:

- An event is committed to the file before any CALL is attempted. The outbox holds at most 256
  events and refuses further ones with `security_outbox_capacity`.
- Events leave the outbox in order, one CALL at a time, and only when the CSMS answers
  `SecurityEventNotification.conf`. Delivery needs a socket whose `BootNotification` was accepted,
  unless `send_before_accepted` is set.
- A timeout, a closed socket or a CALLERROR `InternalError` or `GenericError` keeps the event
  queued. It is resent after one second, on the next accepted connection when the socket died.
- Any other CALLERROR is a final answer: the event is dropped and counted as rejected, so a
  permanently refused event cannot block the queue.
- After a process restart the restarted station registers, then resends every unacknowledged event
  with its original type, timestamp and `techInfo`. An event whose confirmation was lost in the
  crash is therefore delivered at least once.
- `duplicate_acknowledged` queues an identical copy right after each acknowledgement, exercising
  CSMS deduplication. `send_before_accepted` delivers on a socket that has not yet been accepted,
  exercising the CSMS refusal of events from an unregistered station.

The pinned `SecurityEventNotification.req` bounds are enforced locally, with a clear error, before
anything is queued: `type` at most 50 characters (`security_event_type_too_long`), `techInfo` at
most 255 (`security_event_tech_info_too_long`) and `timestamp` an RFC 3339 date-time
(`security_event_timestamp_invalid`). Any other `type` is accepted, so vendor events work. The
section 8 standard names are listed in the independence test.

## Extended trigger

An `ExtendedTriggerMessage` is intercepted as a raw frame, so the pinned schema, not the client
library's typed enum, defines the request. The station replies, writes the `.conf`, and only then
sends the triggered message.

- Unknown fields, an unknown `requestedMessage` or a non-integer `connectorId` return CALLERROR
  `FormationViolation`; a `connectorId` of zero or below returns `PropertyConstraintViolation`.
- The reply is the configured answer. `Accepted` becomes `Rejected` when there is nothing to send:
  an unknown connector, `MeterValues` before any reading, or a `BootNotification` after this
  connection's Boot was accepted (the same rules as `TriggerMessage`).
- `BootNotification`, `Heartbeat`, `MeterValues` and `StatusNotification` reuse the simulator's
  trigger payload builder: the last Boot, the latest status and meter reading with context
  `Trigger`. Without a `connectorId`, `StatusNotification` reports the station and each connector.
- `LogStatusNotification` follows N01.FR.12. With no `GetLog` upload ongoing the status is `Idle`
  without `requestId`. Otherwise it is `Uploading` with that upload's `requestId`. A legacy
  `GetDiagnostics` upload does not count.
- `FirmwareStatusNotification` sends a `SignedFirmwareStatusNotification`. Per L01.FR.28/29 it is
  `Idle` without `requestId` when the last sent status was `Installed` or no signed update ran,
  and otherwise that last sent status with the update's `requestId`.
- `SignChargePointCertificate` is answered `NotImplemented` by default. Configured as `accepted`
  it is acknowledged and nothing follows, since signing is not modelled.
- `reply_delay_ms` holds the `.conf` back, which also holds back the station's own socket handling
  for that time. `omit` accepts the request and sends nothing. A trigger is never resent after a
  reconnect or restart.

## Scenario actions

- `emit_security_event` queues one event. Its `payload` accepts `type` (required), `timestamp`
  (RFC 3339; the current time with millisecond precision when omitted) and `techInfo`. The bounds
  above are checked when the scenario is parsed. The event name is `security_event_queued` and the
  step detail is the outbox identity.
- `assert_security` and `await_security` compare an `expect_response` subset of the safe snapshot:
  `stateAvailable`, `pendingEvents`, `eventsEmitted`, `eventsAcknowledged`, `eventsRejected`,
  `duplicatesSent`, `lastAcknowledgedType`, `triggersReceived` and `lastTrigger` (such as
  `Heartbeat:Accepted`). The event name is `security_observed`. Neither the snapshot nor the traces
  contain `techInfo`.

The firmware model does not emit events by itself (L01.FR.02/03/33); a scenario raises them
explicitly. See `bins/uob-sim/examples/security-1.6*.toml`.

## Not modeled

- **Certificates.** No certificate messages, `SignChargePointCertificate` signing, certificate
  expiry events or key material.
- **Security profile changes and `AuthorizationKey`.** The station never rotates credentials and
  has no security profile; its authentication stays the simulator's `credentials_file`.
- **Security log.** Events are sent, but not stored in a rolling security log (A04.FR.04).
- **Automatic events.** No event is raised by the simulator itself, including reboots and firmware
  updates.
- **Firmware history without a `firmware16` table.** The extended trigger for
  `FirmwareStatusNotification` then always reports `Idle`.

## Verification

```text
cargo test --locked -p uob-sim --test security16_model --test security16_wire \
  --test security16_trigger_wire --test security16_process --test security16_scenario \
  --test security16_independence
```

`security16_independence` checks that the simulator reaches no bridge crate, that emitted events
follow the pinned request schema and the section 8 names, and that every pinned
`MessageTriggerEnumType` and `.conf` status is configurable. The model tests cover ordering,
retry, drops, duplicates, bounds, restart durability and the log and firmware status rules. The
wire tests use a real OCPP-J socket and assert the exact frames:

- an event sent, acknowledged and dequeued, with the next event waiting for it;
- a resend after the acknowledgement was withheld and the connection lost, and delivery of an event
  queued while disconnected;
- the duplicate fault, the pre-acceptance fault with a final CALLERROR, and a retried
  `InternalError`;
- every extended trigger class with accepted, rejected, not implemented, delayed and omitted
  replies, strict validation and a pending Boot.

The process test kills the real binary with an unacknowledged event and checks that the restarted
binary resends it from the durable queue, and that no `techInfo` reaches stdout or stderr.

`bins/uob-sim/tests/security16_joint_smoke.py` drives the real bridge and two simulator stations. It
is opt-in and is not part of `scripts/verify-workspace.sh`; its command and checks are listed in
[OCPP 1.6J security events](../architecture/ocpp16-security-events.md#verification). The bridge-side
station options it needs are documented in
[the headless CLI guide](../operations/headless-cli.md#protected-ocpp-16-security-events-and-extendedtriggermessage).
