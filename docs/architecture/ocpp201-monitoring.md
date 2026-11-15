# OCPP 2.0.1 variable monitoring

The bridge sends the five OCPP 2.0.1 monitoring commands, collects the multipart monitoring
report and journals charger-initiated `NotifyEvent` calls (Part 2 Edition 4, use cases N02 to
N08). A caller configures what a station monitors; the station decides, per item, what it
accepted. Every native status is kept exactly and no monitor is assumed to exist because a
request was sent. The independent `uob-sim` station model, which exercises the same wire with its
own reading of the specification, is described in
[the simulator documentation](../simulator/monitoring201.md). Operators enable the commands as
described in [the headless CLI guide](../operations/headless-cli.md#protected-ocpp-201-variable-monitoring).

## Scope

| Use case | Direction | Bridge behavior |
|---|---|---|
| N02 Get Monitoring Report | CSMS to station, then `NotifyMonitoringReport` | `GetMonitoringReport`; fragments collected by the shared multipart collector |
| N03 Set Monitoring Base | CSMS to station | `SetMonitoringBase` (`All`, `FactoryDefault`, `HardWiredOnly`), native status kept |
| N04 Set Variable Monitoring | CSMS to station | `SetVariableMonitoring`, one native result per monitor |
| N05 Set Monitoring Level | CSMS to station | `SetMonitoringLevel`, native status kept |
| N06 Clear / Remove Monitoring | CSMS to station | `ClearVariableMonitoring`, one native result per ID |
| N07 Alert Event, N08 Periodic Event | Station to CSMS | `NotifyEvent` journaled, always acknowledged |

Charger-only behavior is not the bridge's to enforce and is only observed through replies and
events: which variables are monitorable, thresholds that are out of a variable's range, the
generation of monitor IDs, replacement and persistence of monitors, the severity filter applied
by the current monitoring level (N05.FR.03), whether a delta or periodic monitor fires, and
whether hard-wired monitors can be cleared. The bridge reports what the station says.

## Enabling

Five independent, privileged, default-off station options enable the commands for an OCPP
2.0.1 station: `set_variable_monitoring`, `clear_variable_monitoring`, `get_monitoring_report`,
`set_monitoring_base` and `set_monitoring_level`. Each needs the existing distinct control and
privileged grant files, targets the station root only and is refused on OCPP 1.6J stations.
Callers submit `Ocpp` operations with the **privileged** grant and the native request as the
payload; `payload_schema` is `urn:OCPP:Cp:2:2020:3:<Action>Request`. `NotifyEvent` handling has no
option: it is on for every accepted 2.0.1 station.

## Command validation and refusals

The bridge validates the request against the pinned OCA schema and refuses it before anything is
transmitted (HTTP 400, nothing on the wire) for:

- a severity outside 0 to 9, a negative `Delta` value or an unknown enumeration;
- an empty or duplicated ID list, or duplicated monitor `id` values in one request;
- a **duplicate monitor**: two items of one `SetVariableMonitoring` with the same component,
  variable, type and severity;
- any `customData`, and control characters in text;
- a target other than the station root;
- a monitor `value` the bridge cannot hold exactly (below);
- more items or bytes than the station's learned limits (below).

**Monitor value bound.** Monitor values are exact decimals, read from the original JSON number
text and never through binary floating point. A value is representable only when its scale
(fractional digits less the exponent) is at most 64 and its coefficient fits a signed 128-bit
integer, which is 38 significant digits for any number and 39 only up to
170141183460469231731687303715884105727. Larger magnitudes and scales are not rounded. In a
request such a value is refused before transmission (HTTP 400); in a `NotifyMonitoringReport`
fragment it fails the report as `invalid_fragment`.

**Duplicate-monitor policy.** The fixture corpus positive case `monitoring-set-mixed.json`
deliberately repeats a monitor so that a charger answers `Duplicate` (N04.FR.10). That request is
wire-valid and the simulator model handles it, but the bridge refuses to send it. This is
intended CSMS policy: the reply mapping below identifies each result by component, variable,
type and severity, so a request with two identical identities could not be answered unambiguously.
A caller that wants to observe a charger's `Duplicate` status sets the monitor in two requests.

**Learned limits.** A request carrying more than one monitor or ID is transmitted only within the
station's `MonitoringCtrlr` `ItemsPerMessage` and `BytesPerMessage` (instances
`SetVariableMonitoring` and `ClearVariableMonitoring`). The bridge learns these only from an
operator's accepted `GetVariables` command for exactly those variables, so `get_variables` must
also be enabled for the station; it never queries them itself. The limits live in the
connection's session, so a reconnect must learn them again. Until **both** limits of an action are
known, a multi-item `SetVariableMonitoring` or `ClearVariableMonitoring` is not transmitted and
only single-item requests are; a larger request is refused with HTTP 400 `policy_rejected` and the
lifecycle stage `rejected`. Limits that are not learned are never guessed.

## Reply correspondence

A reply is evidence only when it is valid against the response schema, free of control characters
and corresponds to the request. Anything else ends the command `transmission_uncertain` and is
never replayed after a restart or reconnect.

- `SetVariableMonitoring`: exactly one result per requested monitor, matched on component,
  variable, type and severity in any order (N04.FR.01, N04.FR.02). An accepted
  result must carry the monitor `id` (N04.FR.11), the same `id` when the request replaced one
  (N04.FR.12). A native `id` is retained only for an accepted monitor.
- `ClearVariableMonitoring`: exactly the requested IDs, no more and no fewer, in any order
  (N06.FR.05).
- `SetMonitoringBase`, `SetMonitoringLevel` and the `GetMonitoringReport` acknowledgement carry
  the native `status`.
- Only `statusInfo.reasonCode` is retained (1 to 20 printable ASCII characters);
  `additionalInfo` is validated and dropped.

A command is `accepted` only when the station accepted every item. A single refused monitor makes
the command a protocol rejection (`lifecycle.accepted` false, stage `protocol_response`) while
each per-item result stays visible in `monitoring_201.outcome.monitors`.

## Result evidence

`CommandResult.monitoring_201` (contract revision 18, `V1_MONITORING_201`) holds the immutable
`query` (`action` and the native request fields) and the `outcome`:

| `outcome.kind` | Content |
|---|---|
| `set_monitors` | `monitors`: per monitor `status`, optional `id`, optional `reason_code`, type, severity, component, variable |
| `clear_monitors` | `monitors`: per ID `id`, `status` (`Accepted`, `NotFound`, `Rejected`), optional `reason_code` |
| `base`, `level` | native `status` |
| `report` | `ack` (the native acknowledgement) and `report` (the collection state below) |

## Report collection

After an `Accepted` `GetMonitoringReport` acknowledgement the result is stored with
`report.state = pending`, and the station's `NotifyMonitoringReport` calls are collected by the
shared bounded collector described in [multipart reports](multipart-reports.md). The state then
becomes one of:

- `complete`: `fragments` (sequence, `tbc`, generated time and monitor count per part) and
  `monitors` (each monitor with its component, variable and monitoring entries);
- `incomplete` with a `reason`;
- `not_expected`: the station did not accept the request (`NotSupported`, `EmptyResultSet`,
  `Rejected`) or the command never reached it, so no report will arrive.

Shared limits: 1 MiB of report items, 4096 items, 256 fragments, and 30 s from the actual send of
the request. Items are charged to the shared runtime budget before they are sanitized. Sequence
numbers must start at 0 and advance by exactly one.

| `reason` | Cause |
|---|---|
| `out_of_order_sequence`, `duplicate_sequence` | A fragment skipped ahead or repeated or lowered its `seqNo`; identical content is still a conflict |
| `byte_limit`, `item_limit`, `fragment_limit` | A shared collection limit was exceeded |
| `capacity` | The runtime budget could not hold the report |
| `timeout` | No final fragment within the deadline |
| `disconnected` | The socket closed mid-report |
| `interrupted` | The daemon restarted while the report was pending |
| `correlation` | A monitor outside the requested `componentVariable` filter or criteria was reported |
| `invalid_fragment` | A fragment failed decoding or sanitizing |
| `output_limit` | The finished result would exceed the 1 MiB result limit |
| `storage_unavailable` | The final state could not be stored |

Monitors that fit the first 1 MiB of the result are kept; later ones are counted in
`monitors_omitted`, never dropped silently. A well-formed fragment for a request
that is not being collected (unsolicited or already retired) is acknowledged with the empty
response and discarded. A malformed fragment of a collected request ends it `invalid_fragment`.
Until the session has bound the durable report store a stray fragment gets `NotImplemented`.

### Storage and restart

The report state is kept in `command_results.payload` with the `report_pending` flag. The
collector finishes a report with the full evidence (request echo, native acknowledgement and
terminal report) and the lifecycle the acknowledgement resolves, so it does not matter whether it
or the application writes first. In one SQLite transaction the first terminal report wins: a row
still admitted or dispatched is resolved and given the evidence, a pending report is replaced, and
a terminal report is returned untouched (the caller can tell it from an unknown request, which
returns nothing). The application's later write of the `pending` acknowledgement adopts the
finished report and cannot move the lifecycle backwards. SQLite `user_version` is 22 for this feature: no DDL changed, but an older binary that
does not understand `monitoring_201` is fenced out, so downgrade is refused rather than silently
reading a result it cannot represent. At startup, a pending report whose collector cannot have
survived the restart becomes `incomplete` with `interrupted`. A report is never resumed.

### Capacity shared with other report kinds

`GetBaseReport`, `GetReport`, `GetChargingProfiles` and `GetMonitoringReport` use separate
request-ID namespaces (`NotifyMonitoringReport` is its own kind), so the same signed ID can be
open once in each. All of them share the four route slots per connection and the 4096-entry
used-ID set. A fifth concurrent report on one connection is refused before the wire.

## NotifyEvent pipeline

A `NotifyEvent` call (N07, N08) is handled as follows. It is always on for accepted stations.

1. **Decode.** The call is validated against the pinned schema and sanitized. The native values
   are kept: `eventId`, `timestamp`, `trigger`, `actualValue` (at most 2500 characters),
   `eventNotificationType`, `variableMonitoringId`, component, variable, `cause`, `techCode`
   (50), `techInfo` (500) and `transactionId` (36). Every schema-valid call the typed record can represent is
   accepted, because
   the CSMS answers it with the empty response (N07.FR.03, N08.FR.02) and a charger retries a
   refused event, so one refusal could stall its whole queue. Control characters in charger text
   are replaced one for one with U+FFFD (the length in characters is unchanged), and a missing
   `variableMonitoringId` stays absent whatever the notification type. Only what the typed
   record cannot represent is refused with the native CALLERROR, changing nothing: an integer
   outside 32 bits (`eventId`, `cause`, `variableMonitoringId`, EVSE and connector ids), an EVSE
   or connector id that is not positive, a `seqNo` beyond 32 bits unsigned, and a timestamp that
   is not an instant. A station that is not yet registered is refused with `ProtocolError`
   before it can spend any of its budget.
2. **Gate.** Each configured station has one gate for the life of the daemon process, with a
   budget of 60 journal records and 1 MiB per 60 s window. Journal events cannot be shed by
   retention, so the budget is not refreshed by a reconnect, and shed counts not yet reported
   are kept across it. A new connection only restarts the ordering state (a new series starts
   at `seqNo` 0). The gate judges ordering (`seqNo` as `in_sequence`, `restarted` when 0 appears mid-series, or
   `gap` with the expected number) and charges the budget before the commit, so a storage
   failure cannot be used to bypass the flood limit. One record keeps at most 32 events and
   64 KiB in native order; the rest is counted in `events_omitted`.
3. **Atomic journal record.** An admitted call becomes one `station.monitoring_event.201` journal
   record, committed before the response is sent. Its payload is
   `monitoring_events_201 = {generated_at, seq_no, tbc, ordering, events, events_omitted?,
   shed_calls?, shed_events?}`.
4. **Response.** The station receives the empty `NotifyEventResponse` (`{}`).

**Shed policy.** A call over the budget is acknowledged with `{}` and shed: nothing is written,
the ordering advances (the charger was acknowledged and will not repeat it), and the shed calls
and events are counted. The next admitted record reports `shed_calls` and `shed_events`. The
shed also increments the shared dropped-diagnostics counter, visible as
`uob_dropped_diagnostics_total` at `GET /metrics` and `runtime.dropped_diagnostics` in
`GET /api/v1/health`. A station flood therefore costs the bridge a bounded amount of work per
minute and cannot close the session.

**Storage budget limitation.** The gate bounds the write rate, not the total. Journal events
count against the shared operational storage budget and are kept for the seven-day retention
window, so a station that sustains the full budget (about 1 MiB per minute) can exhaust the
256 MiB default budget in a few hours. Routine and new-session writes then fail for every
station until rows age out. Other ungated station journal writes, such as `MeterValues`, share
this exposure. The counter above is shared and carries no station label; the per-station shed
counts are in that station's next journal record.

Journal events are available at `GET /api/v1/events?station_id=<id>&types=station.monitoring_event.201`
as server-sent events with the read grant. A late subscriber replays the retained records.

## Handling of charger text

Component, variable, instance and event text are charger-supplied. They are length-limited by
the schema and the bounds above and placed in typed fields. In a `NotifyEvent` control characters
are replaced with U+FFFD, so the event is still journaled and acknowledged. Everything else the
bridge must answer or reconcile is refused instead: control characters in a command request, in a
native reply (which ends the command `transmission_uncertain`) and in a report fragment (which
ends the report `invalid_fragment`). Trace records of the event path carry only counts, the action
and station identity, never event contents. `statusInfo.additionalInfo` and `customData` are never
retained.

## Contract and schema versions

| Surface | Version |
|---|---|
| `ContractVersion` revision | 18 (`V1_MONITORING_201`) |
| Command-result public schema | v1.18 |
| Export schemas | v1.19 |
| SQLite `user_version` | 22 (downgrade fence, no DDL) |

See [JSON schema versioning](../contracts/json-schema-versioning.md).

## Known limitations

- **Shed events appear as a rejected flow event.** When the gate sheds a call, the capture trace
  records a rejected (policy-denied) flow event, because the flow API has no "dropped" outcome.
  The durable pressure evidence is the shed counts in the next journal record and the
  dropped-diagnostics counter.
- **Unknown request at finish.** The command row exists before the request is dispatched, so a
  collector that finishes before the application records the acknowledgement still stores the
  report. Only a request whose row has been lost (for example pruned) is unknown; the collector
  does not retry it, records a failed durable commit in the trace and the report is not stored.
- **No resume.** A report cut short is never resumed or re-requested; the caller submits a new
  `GetMonitoringReport` with a fresh `requestId`.
- **Monitors are the charger's.** The bridge keeps no model of which monitors exist; a report is
  the only authoritative list.
- **Multi-item requests need learned limits** (see above).

## Not covered

- Security events and `SecurityEventNotification` (#128).
- Customer information (#133).
- Event-driven automation, alerting or forwarding of journaled events.
- Hardware stations. Interoperability was shown only with the independent simulator and the
  fixture corpus. This is not an OCA certification claim.

## Verification

```text
cargo test --locked -p uob-contracts monitoring
cargo test --locked -p uob-application --lib monitoring201
cargo test --locked -p uob-application --test monitoring201
cargo test --locked -p uob-storage-adapter --test monitoring201
cargo test --locked -p uob-protocol-adapter --test ocpp201_monitoring --test command_registry
cargo test --locked -p uob-service --test monitoring201
cargo test --locked -p uob-sim --test monitoring201_model --test monitoring201_wire \
  --test monitoring201_process --test monitoring201_scenario --test monitoring201_independence
cargo build -p uob-service -p uob-sim
python3 -B bins/uob-sim/tests/monitoring201_joint_smoke.py --bridge target/debug/uob \
  --simulator target/debug/uob-sim --output <fresh private directory>
```

The joint smoke is opt-in and is not part of `scripts/verify-workspace.sh`. It starts a real
daemon and two independent simulator stations. It shows that a multi-item request is refused
until the limits are learned, three monitors are accepted with per-item IDs, and a report listing
five monitors arrives in three fragments and ends `complete`. An event burst is acknowledged and
visible as four journal records. A negative `Delta` and a severity of 10 are refused by the
bridge, an unknown component is refused per item by the charger, and a clear reports `Accepted`,
`NotFound` and the hard-wired refusal. A report filtered on the cleared monitor's component ends
`not_expected`, and the second station's report, cut short by a closed socket, ends `incomplete`
with `disconnected`. The corpus requirement mapping is in
[the fixture corpus guide](../testing/ocpp-fixture-corpus.md#ocpp-201-monitoring-fixtures).
