# Native OCPP 2.0.1 reservation software peer

`uob-sim` owns a separately typed OCPP 2.0.1 charging-station reservation model
(`uob_sim::reservation201`). It is derived from the OCA OCPP 2.0.1 Part 2 FINAL
functional block H (use cases H01–H04) and Errata v1.0 §10, not from bridge
types, encoders or decisions, and it does not reuse the OCPP 1.6 reservation
semantics. The crate links no bridge crate; `reservation201_independence`
asserts that.

## Private station configuration

Use `bins/uob-sim/examples/reservations-2.0.1-config.toml` and
`bins/uob-sim/examples/reservations-2.0.1.toml` as synthetic examples. Copy them
into a **new**, owner-only directory (`0700`) with configuration, credential and
scenario files at `0600`, and replace the endpoint and absolute private paths.

```toml
[stations.reservation201]
private_state_file = "/absolute/private/reservations.json"  # owner-only 0700 parent
enabled = true              # ReservationCtrlr Available/Enabled; false => Rejected (H01.FR.01)
non_evse_specific = true    # ReservationCtrlr.NonEvseSpecific (H01.FR.18/19)
connector_types = [{ evse = 1, connector = 1, type = "cType2" }]
```

`connector_types` names configured EVSE/connector pairs with pinned
`ConnectorEnumType` values; an unconfigured connector matches no requested
`connectorType`. The table is accepted only on `ocpp_version = "2.0.1"`
stations (`[stations.reservation16]` only on 1.6 stations); a mismatch fails
setup with `invalid_reservation_edition`.

The private file binds format, exact station, protocol `ocpp2.0.1` and the
configured EVSE/connector topology. It reuses the 1.6 reservation storage
rules: owner/mode/inode checks, an exclusive writer lock, atomic replacement and
directory synchronization before a reply. Uncertain synchronization makes the
model unavailable (`stateAvailable=false`) until a successful reopen. Limits are
128 reservations and 256 undelivered status updates.

## Native semantics

`ReserveNow` and `CancelReservation` are validated against copies of the pinned
OCA request schemas (`bins/uob-sim/src/reservation201/*.json`) and the same
IdToken rules as the 2.0.1 local list; violations answer `FormationViolation`
and never reach the model.

- **ReserveNow** answers from actual connector state: Rejected when not enabled,
  for an unknown EVSE, an absent connector type, an elapsed expiry, or no
  `evseId` without NonEvseSpecific; Faulted/Unavailable when all targeted
  connectors are; Occupied when the EVSE is occupied or already reserved
  (errata H01.FR.11) or the capacity is needed by unspecified reservations.
  The same `id` replaces the previous reservation only on Accepted; a refused
  replacement keeps it (H01.FR.02).
- **Unspecified EVSE / connectorType** reservations stay unbound: every one
  keeps a distinct usable EVSE (bipartite matching, H01.FR.07/09). An EVSE the
  unbound set cannot spare reports `Reserved` (errata H01.FR.20/24).
- **Reported view**: after the reply, every connector of an exactly reserved
  EVSE reports `StatusNotification(Reserved)` (errata H01.FR.23); cancellation,
  expiry or a start reports `Available` again. The station's own explicit
  StatusNotification for an EVSE is never duplicated.
- **CancelReservation**: Accepted for a current id (H02.FR.02), Rejected for an
  unknown one (H02.FR.01); no ReservationStatusUpdate (H02 remark).
- **Expiry** is inclusive and runs every 50 ms whether or not a CSMS socket
  exists; at restart it is applied before registration. It reports
  `StatusNotification(Available)` and then
  `ReservationStatusUpdate(Expired)` (H04.FR.01–03).
- **Faulted/Unavailable target** (actual StatusNotification sent by the
  scenario): exact reservations on that EVSE, and unspecified reservations whose
  capacity is gone, are cancelled and reported `Removed` (H01.FR.16/17). An
  Occupied report on reserved capacity is refused before the CALL is sent.
- **Use (H03)**: a `TransactionEvent(Started)` with an authorized idToken
  matching the reservation's idToken (type plus case-insensitive value) or whose
  actual groupIdToken matches the reservation's groupIdToken ends the
  reservation, and the event carries `reservationId` (H01.FR.15, E01.FR.13). The
  group comes from the 2.0.1 local list, then the unexpired cache, else a real
  `Authorize` to the CSMS (H03.FR.07/08). A non-matching or tokenless start on
  an exactly reserved EVSE is refused before the CALL.

Status updates live in a durable outbox and leave it only after the correlated
`ReservationStatusUpdateResponse`; reported views are transient. Neither is sent
before an accepted Boot on the current socket, while an intercepted inbound
reply is pending, or while a station CALL that caused them is in flight. A
failed delivery retries after one second. Raw idTokens and groupIdTokens stay in
the private file; `snapshot()` and scenario output expose only
`stateAvailable`, `activeReservations`, `revision`, `pendingUpdates` and
`reservations[{id, evseId, connectorType, expiryDateTime}]`.

## Scenario steps and faults

- `await_reservation` / `assert_reservation` with an `expect_response` of the
  safe snapshot keys above (polling vs. immediate).
- `status` with an actual `connectorStatus` (`Faulted`, `Unavailable`,
  `Occupied`, `Available`) drives H01.FR.16/17.
- `authorize` then `start_transaction` with `eventType = "Started"`, `evse`
  and `idToken` uses a reservation; an authored `reservationId` must equal the
  reservation actually ended.
- `delay_local_reply` (`duration_ms` 1..=30000) and `drop_local_reply` apply to
  the next ReserveNow/CancelReservation reply after the model has committed;
  replies beyond the bridge deadline need an external holding relay, as in the
  1.6 joint smoke.

These are software-only semantics checks; no hardware, OCA certification or
production listener behaviour is claimed.
