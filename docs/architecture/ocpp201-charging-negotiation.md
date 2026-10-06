# OCPP 2.0.1 charging needs and external limits

OCPP 2.0.1 stations send four smart-charging notifications that the CSMS must answer:
`NotifyEVChargingNeeds` and `NotifyEVChargingSchedule` (K15–K17, ISO 15118 load leveling and
renegotiation) and `NotifyChargingLimit` and `ClearedChargingLimit` (K11–K14, limits imposed
by an external system such as a DSO or a home energy manager). The bridge records each one
and answers within its own authority. It never calculates a schedule or enforces a limit,
and it never turns an observation into a command.

## Decoding

`v201::charging_negotiation` validates each CALL against its pinned OCA schema and then
against Part 2 semantics the schema cannot express. Any failure is a
`PropertyConstraintViolation` CALLERROR with nothing recorded.

- `NotifyEVChargingNeeds` needs a positive `evseId` and exactly one parameter set that
  matches `requestedEnergyTransfer`: AC parameters for the three AC modes, DC parameters
  for `DC` (K15.FR.06, K17.FR.06). Integers must be non-negative, `evMinCurrent` must not
  exceed `evMaxCurrent`, and `maxScheduleTuples` must be positive when present.
- `NotifyEVChargingSchedule` needs a positive `evseId` and one schedule whose periods start
  at zero, strictly increase and carry exact non-negative tenths, using the same parser as
  composite schedules and profile reports.
- `NotifyChargingLimit` refuses a `CSO` source (K11.FR.05, K12.FR.04) and a zero `evseId`.
  An absent `evseId` addresses the grid connection.
- `ClearedChargingLimit` accepts any source. An absent or zero `evseId` addresses the grid
  connection, and the reported value is kept as sent.

`customData` is never retained. An ISO 15118 `salesTariff` is omitted and flagged.

## Answers

| Notification | Answer |
|---|---|
| Charging needs, EVSE not configured | `Rejected` with `UnknownEvse` |
| Charging needs, no single current transaction on the EVSE | `Rejected` with `TxNotFound` |
| Charging needs, default policy | `Rejected` with `NotEnabled` (K15.FR.04, K17.FR.04) |
| Charging needs, `ev_charging_needs_processing = true` | `Processing` (K15.FR.05, K17.FR.05) |
| EV schedule | `Accepted` or `Rejected`, see below |
| Charging limit, cleared limit | empty response |

The bridge never answers charging needs with `Accepted`, because it has no schedule of its
own to send within 60 seconds. `Processing` is an operator assertion that its EMS will send
a `TxProfile` through this bridge later, either a canonical `SetChargingLimit` or a native
`SetChargingProfile` (K15.FR.07/08). The station then charges within its own composite
schedule and renegotiates when the profile arrives (K16). The option is OCPP 2.0.1 only. It
requires `allow_charging_limit` or `set_charging_profile` and an enabled command path. The
bridge sends no profile and starts no renegotiation on its own (K15.FR.13, K16.FR.08/12,
K17.FR.13).

### Checking an EV schedule

The CSMS checks an EV charging schedule against its own charging schedule (K15.FR.11/12,
K16.FR.06/07, K17.FR.11/12). The bridge's own schedule is the set of `TxProfile`s it
installed for the EVSE's current transaction. They come from the durable ownership ledger
(`ChargingProfileStore201::charging_profile_owners`), and each is rebuilt from its retained
command. A canonical charging limit is a single unbounded relative period at stack level
zero.

- No such profile: `Accepted`, basis `no_csms_schedule`.
- Every interval of the EV schedule, anchored at `timeBase` and truncated by its duration,
  stays within the limit of the highest active stack level: `Accepted`, basis
  `within_csms_schedule`. Validity windows and absolute schedule durations bound each
  profile, and outside them no bridge limit applies.
- Any interval exceeds that limit: `Rejected` with `ValueTooHigh`, basis
  `exceeds_csms_schedule`.
- The limits cannot be placed exactly: `Rejected` with `Unspecified`, basis `unverifiable`.
  This covers an admitted, in-flight or uncertain profile mutation, an unretained or
  unparseable request, a unit other than the EV schedule's, a recurring profile, and a
  relative profile with more than one period or a duration. A relative profile starts at the
  station's PowerPathClosed moment, which the bridge does not observe.

Amperes and watts are never converted using a guessed voltage or phase count.

## Durable state

Each record commits atomically before the station gets its answer. The commit writes the
latest-state snapshot points and one typed journal event. Snapshot points reach every
selected target through the normal one-second latest-state publication. The journal event
reaches management and EMS/SCADA event streams.

| Points | Location |
|---|---|
| `ocpp201/evse-{n}/ev-charging-needs/*` | EVSE resource: `status`, `reason`, `transaction_id`, `requested_energy_transfer`, `departure_time`, `max_schedule_tuples`, `energy_amount_wh`, `ev_min_current_a`, `ev_max_current_a`, `ev_max_voltage_v`, `ev_max_power_w`, `state_of_charge_percent`, `ev_energy_capacity_wh`, `full_soc_percent`, `bulk_soc_percent` |
| `ocpp201/evse-{n}/ev-charging-schedule/*` | EVSE resource: `status`, `basis`, `reason`, `transaction_id`, `time_base`, `schedule_id`, `charging_rate_unit`, `duration_seconds`, `period_count` |
| `ocpp201/charging-limit/{source}/*` | Station: `active`, `grid_critical`, `schedules` |
| `ocpp201/evse-{n}/charging-limit/{source}/*` | EVSE resource: the same three fields |

Every field is written on each notification, so values from an earlier AC report never
linger after a DC report. A release sets `active` to false and makes the other two fields
unavailable. Releases apply only to their own source and scope. An EVSE-scoped limit must
name an exactly configured EVSE resource; otherwise it is a `ProtocolError` CALLERROR.
Needs and EV schedules for an unconfigured EVSE are answered and journaled without points.

The journal event types are `station.ev_charging_needs.201`,
`station.ev_charging_schedule.201`, `station.charging_limit.201` and
`station.charging_limit_cleared.201`. The payload is the `ChargingNegotiation201` station
event, with the exact needs, the schedules and the bridge's answer. A record that would
exceed `CHARGING_NEGOTIATION_EVIDENCE_LIMIT_201` (32 KiB) omits the schedules but keeps
their count, so it always fits the 64 KiB integration event bound. Older readers decode the
event as an `Invalidation` marker, so it needs no SQL migration and no new published JSON
schema revision.

## Loop safety

These handlers never admit a command. A negotiation event is explicitly excluded from
observed-effect reconciliation. A repeated limit therefore produces no command, no observed
command effect and no CALL to the station. Targets receive the limits as observed snapshot
state only.

```text
cargo test --locked -p uob-contracts --test charging_negotiation201
cargo test --locked -p uob-application --test charging_negotiation201
cargo test --locked -p uob-protocol-adapter --test ocpp201_charging_negotiation
cargo test --locked -p uob-storage-adapter --test charging_negotiation201 --test charging_profile201
cargo test --locked -p uob-ocpp-fixtures --test charging_negotiation201
cargo test --locked -p uob-service --test charging_negotiation201
```

The service suite runs the actual daemon with an independent WebSocket peer and an HTTP
client. It covers policy answers, the bridge's own canonical limit, an exceeding EV
schedule with no follow-up CALL, refused notifications, releases and restart. These
commands are verification instructions, not a claim they ran. No ISO 15118 stack, hardware
interoperability or OCA certification is claimed.
