# Independent OCPP 2.0.1 monitoring station model

`uob-sim` can act as an OCPP 2.0.1 charging station whose variables are monitored by a CSMS. The
model is derived independently from the pinned OCA sources, not from the bridge implementation:

- OCPP 2.0.1 Edition 4 Part 2, use cases N02 (Get Monitoring Report), N03 (Set Monitoring Base),
  N04 (Set Variable Monitoring), N05 (Set Monitoring Level), N06 (Clear / Remove Monitoring),
  N07 (Alert Event) and N08 (Periodic Event), plus the `MonitoringCtrlr` variables of the
  referenced-components appendix.
- The byte-exact `SetVariableMonitoring`, `ClearVariableMonitoring`, `GetMonitoringReport`,
  `NotifyMonitoringReport`, `SetMonitoringBase`, `SetMonitoringLevel` and `NotifyEvent` request and
  response schemas in `tests/ocpp-fixtures/corpus/schemas/2.0.1`.

A station with a `monitoring201` table answers the five monitoring CALLs and sends
`NotifyMonitoringReport` and `NotifyEvent`. Stations without the table keep the client library's
CALLERROR `NotImplemented`. The model is configuration-seeded and lives in memory for as long as
the station runs: monitors are not restored by a process restart (N04 requires persistence on a
real station; the simulator starts again from its seed).

## Configuration

```toml
[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/ocpp/alpha"
ocpp_version = "2.0.1"
reconnect = true                # events and reports continue after the station registers again

[[stations.evses]]
id = 1
connectors = [1]

[stations.monitoring201]
items_per_message_set = 4       # ItemsPerMessage/SetVariableMonitoring, 1..=256
bytes_per_message_set = 8192    # BytesPerMessage/SetVariableMonitoring, at least 64
items_per_message_clear = 4     # ItemsPerMessage/ClearVariableMonitoring, 1..=256
bytes_per_message_clear = 1024  # BytesPerMessage/ClearVariableMonitoring, at least 16
max_monitors = 32               # 1..=256 monitors at once (ActiveMonitors maxLimit)
monitors_per_fragment = 2       # 1..=256 monitors per NotifyMonitoringReport
events_per_message = 2          # 1..=256 events per NotifyEvent
initial_level = 9               # 0..=9 until the first SetMonitoringLevel
supported_criteria = ["ThresholdMonitoring", "DeltaMonitoring", "PeriodicMonitoring"]
supported_bases = ["All", "FactoryDefault", "HardWiredOnly"]
# Fault controls (see below):
report_fault = "none"
set_reply_fault = "none"

[[stations.monitoring201.variables]]
component = { name = "Connector", evse = { id = 1, connectorId = 1 } }
variable = { name = "Temperature" }
data_type = "decimal"           # decimal (default), integer, boolean or string
value = "40"                    # initial Actual value
min = -40.0                     # optional minLimit/maxLimit bound thresholds (N04.FR.06)
max = 150.0
monitor_types = ["UpperThreshold", "LowerThreshold", "Delta"]  # empty: not monitorable

[[stations.monitoring201.monitors]]
id = 4003                       # unique, positive
kind = "hard_wired"             # or "preconfigured"
component = { name = "Connector", evse = { id = 1, connectorId = 1 } }
variable = { name = "Temperature" }
type = "UpperThreshold"
value = 120.0
severity = 1                    # 0..=9
```

The table is rejected for an OCPP 1.6 station. Names are at most 50 characters and compared case
insensitively. A seeded monitor must name a configured variable that supports its type and have a
value its variable accepts. The `MonitoringCtrlr`, `LocalAuthListCtrlr` and `AuthCacheCtrlr`
components cannot be configured as monitored variables, because the simulator already answers them.
See `bins/uob-sim/examples/monitoring-2.0.1*.toml`.

## Station behavior

Every request is validated strictly against its pinned schema. A payload outside the schema (unknown
field, wrong type, empty list, missing field, an integer outside `int32`, more than three
`monitoringCriteria`) is answered with CALLERROR `FormationViolation` and changes nothing.

### SetVariableMonitoring (N04)

One result per item, in order, carrying the item's own component, variable, type and severity
(N04.FR.01/02). Each item is judged in this order:

1. `UnknownComponent` when no configured variable has the component, `UnknownVariable` when the
   component lacks the variable, `UnsupportedMonitorType` when the variable does not support the
   type (N04.FR.03-05).
2. `Rejected` with a `statusInfo.reasonCode` for: a severity outside 0..=9 (`InvalidSeverity`); an
   `UpperThreshold`/`LowerThreshold` outside `min`..=`max` (`ValueOutOfRange`, N04.FR.06); a `Delta`
   that is zero or negative (`InvalidDelta`, N04.FR.14/20); a periodic interval that is not
   positive (`InvalidInterval`).
3. For an item with an `id`: `Rejected` when no monitor has that id (`UnknownId`, N04.FR.13), when
   the component or variable differs from the monitor's (`ComponentMismatch`, N04.FR.16) or when the
   monitor is hard-wired (`HardWiredMonitor`, N04.FR.18). Otherwise the monitor is replaced and
   keeps its id (N04.FR.12); a replaced preconfigured monitor becomes custom (N04.FR.15).
4. `Duplicate` when another monitor of the same variable has the same type and severity
   (N04.FR.10).
5. A new monitor gets a generated id, never reused. A full station answers `Rejected` with
   `NoCapacity`.

More items than `items_per_message_set` returns CALLERROR `OccurrenceConstraintViolation`; a
payload longer than `bytes_per_message_set` bytes returns `FormatViolation` (N04.FR.09).

### ClearVariableMonitoring (N06)

One result per id: `Accepted`, `NotFound` or `Rejected` with `reasonCode` `HardWired`
(N06.FR.01-03/05). Clearing an exceeded threshold sends no `cleared` event (N07.FR.12). The same
item and byte limits apply (N06.FR.06/07).

### SetMonitoringBase (N03)

- `HardWiredOnly` removes every custom and preconfigured monitor (N03.FR.05).
- `FactoryDefault` removes custom monitors, including replaced preconfigured ones, and restores
  the preconfigured monitors with their factory settings (N03.FR.04).
- `All` activates the preconfigured monitors again and keeps custom ones (N03.FR.03).

A base missing from `supported_bases` is answered `NotSupported` and changes nothing (N03.FR.02).

### SetMonitoringLevel (N05)

`Accepted` for 0..=9, otherwise `Rejected` (`OutOfRange`) keeping the previous level. Events from
monitors with a severity above the level are not sent (N05.FR.03, N07.FR.15), but the monitors
still track the situation.

### GetMonitoringReport (N02)

- `NotSupported` when `monitoringCriteria` names a criterion missing from `supported_criteria`
  (N02.FR.02); `EmptyResultSet` when nothing matches (N02.FR.10); otherwise `Accepted`.
- `ThresholdMonitoring` selects `UpperThreshold` and `LowerThreshold`, `DeltaMonitoring` selects
  `Delta`, `PeriodicMonitoring` selects `Periodic` and `PeriodicClockAligned` (N02.FR.12-14).
  Criteria and `componentVariable` apply together (N02.FR.05).
- A `componentVariable` entry without `variable` selects every variable of the component, without
  variable `instance` every instance, without component `instance` or `evse` every one, and
  without `connectorId` every connector (N02.FR.16-21).
- After the reply the station sends `NotifyMonitoringReport` CALLs with the request's `requestId`
  (N02.FR.04), `seqNo` counting from 0 (N02.FR.09) and `tbc` until the last one, each holding
  `monitors_per_fragment` monitors in id order. Monitors of one variable share a `monitor` entry.
  A socket that has not registered, or more fragments than the 128 tracked unanswered calls,
  answers `Rejected`.

### Events (N07, N08)

Scenario steps change variable values; each monitor judges the change:

- `UpperThreshold`/`LowerThreshold` send trigger `Alerting` when the value passes the threshold
  (not when it equals it, N07.FR.16/17) and again with `cleared: true` when it returns (N07.FR.02).
  A new or changed threshold is judged against the current value at once (N07.FR.11).
- `Delta` sends trigger `Delta` when a numeric value moved more than the monitor's value from the
  value at setting time or the last notice (N07.FR.18); other variable types report every change
  (N07.FR.19).
- `Periodic` fires every `value` seconds after it was set or last fired; `PeriodicClockAligned`
  at the next multiple of `value` seconds since the epoch, with trigger `Periodic` (N08.FR.06/07).

Each `eventData` carries `eventId`, `timestamp`, `trigger`, `actualValue`, the component, the
variable, `variableMonitoringId` and an `eventNotificationType` of `HardWiredMonitor`,
`PreconfiguredMonitor` or `CustomMonitor` by the monitor's current owner (N07.FR.06). One trigger
chain starts at `seqNo` 0 (N07.FR.07); a larger chain is split over `events_per_message` events per
message with `tbc`. Messages leave the queue in order, one in flight, only on a socket whose Boot
was accepted. A CALLRESULT counts as acknowledged, a CALLERROR or undecodable reply as received
and rejected, and neither is resent; a transport failure resends after one second. Up to 4096
queued messages are held; a larger backlog drops new events.

### Device model

With a monitoring model, `GetVariables`, `GetBaseReport` and `GetReport` also cover
`MonitoringCtrlr` (`Available`, `Enabled`, `ItemsPerMessage` and `BytesPerMessage` with instances
`SetVariableMonitoring` and `ClearVariableMonitoring`, `ActiveMonitoringBase`,
`ActiveMonitoringLevel`, `ActiveMonitors`) and every configured variable with its current value.
`supportsMonitoring` is true exactly for configured variables with monitor types.
`ConfigurationInventory` omits them because they are read-only; `SummaryInventory` includes
`Available` and `Enabled`.

## Fault controls

These make the station misbehave on purpose, for bridge-side negative scenarios.

`report_fault` shapes a `GetMonitoringReport` the station accepted:

| Value | Effect |
| --- | --- |
| `none` | A correct report. |
| `skip_seq_no` | The second and later fragments are numbered one higher: `0, 2, 3`. |
| `duplicate_seq_no` | The second and later fragments are numbered one lower: `0, 0, 1`. |
| `omit_final_fragment` | The final fragment is never sent, so the `tbc` chain never ends. |
| `disconnect_mid_report` | The socket closes after the first fragment (before it for a one-fragment report). |
| `fragments_before_reply` | Every fragment is sent before the `GetMonitoringReport` reply. |

`set_reply_fault` shapes a `SetVariableMonitoring` reply: `wrong_result_count` appends one extra
result (N04.FR.01) and `mismatched_component` names another component in the first result
(N04.FR.02).

## Scenario actions

- `await_monitoring` and `assert_monitoring` compare an `expect_response` subset of the safe
  snapshot (`await_` polls until the step timeout). Event name: `monitoring_observed`.
  - `base`, `level`, `monitorCount`, `hardWired`;
  - `monitorIds` and `monitorTypes` (in id order), `monitors` (id, type, kind, severity, value,
    component and variable names), `variables` (component, variable and current value);
  - `eventsQueued`, `eventsEmitted`, `eventsAcknowledged`, `eventsRejected`, `eventsSuppressed`,
    `lastEventOutcome` (`Result` or `CallError`);
  - `reportsDelivered` (reports whose every fragment was sent), `reportFragmentsSent` and
    `lastReport` (`requestId`, `planned`, `sent`, `outcome`: `Complete`, `Withheld`,
    `Interrupted` or `Failed`).
- `set_monitored_value` with `payload = { component = {...}, variable = {...}, value = "95" }`
  changes a variable and queues the events its monitors raise. Event name:
  `monitored_value_set`.
- `emit_monitoring_events` with `payload = { component, variable, count = 5, monitorId = 4031 }`
  queues `count` (1..=1000) events from the monitor (the first monitor of the variable without
  `monitorId`) in one chain. Event name: `monitoring_events_emitted`.

## Not modeled

- **Persistence across restarts.** The seed is applied again (N04 remark, N07.FR.13).
- **Offline queuing by severity.** `OfflineMonitoringEventQueuingSeverity` is not exposed; events
  queue until the station registers (N07.FR.04, N08.FR.03).
- **Event causes, `techCode`/`techInfo`, `transactionId` and write-only variables.** Not
  generated (N07.FR.05/10); `transaction` monitors are stored and reported but not gated.
- **`ItemsPerMessageGetReport`.** `GetMonitoringReport` is not limited by it (N02.FR.07).
- **Writable monitoring variables.** `MonitoringCtrlr` variables are read-only.

## Verification

```text
cargo test --locked -p uob-sim --test monitoring201_model --test monitoring201_wire \
  --test monitoring201_process --test monitoring201_scenario --test monitoring201_independence
```

`monitoring201_independence` checks that the simulator reaches no bridge crate. It drives the model
with the independently authored requests and negative cases from
`tests/ocpp-fixtures/corpus/wire/2.0.1/monitoring-*.json`, checks the model's replies against the
corpus replies, that the schema interpreter agrees with every corpus negative case, and that every
emitted status, trigger and notification type is in the pinned enumerations.

The model tests cover setup, rejection reasons, replacement, clearing, bases, levels, limits,
report selection and fragmenting, every fault, thresholds with `cleared`, deltas, periodic and
clock-aligned monitors, level suppression and bursts. The wire tests use a real OCPP-J socket for
setup and removal, multi-fragment reports, every fault, event acknowledgement and `CALLERROR`,
level suppression, invalid requests and the device-model variables. The process tests run the real
binary against a scripted CSMS, including a failing assertion and a report cut short by a closed
socket.

## Joint smoke

An opt-in proof against the actual `uob` daemon, not part of the workspace verifier:

```text
cargo build -p uob-service -p uob-sim
python3 bins/uob-sim/tests/monitoring201_joint_smoke.py \
  --bridge target/debug/uob --simulator target/debug/uob-sim --output <fresh dir>
```

Independent simulator stations connect to one daemon. The proof sets monitors and lists them in a
multi-fragment report, acknowledges an event burst, has an invalid monitor refused, clears a
monitor and lists the report again, and ends a report that a station interrupts by closing its
socket as `Incomplete`. The script's bridge field names are collected in its `BRIDGE_INTERFACE`
block.
