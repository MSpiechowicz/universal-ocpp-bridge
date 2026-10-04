# Deterministic simulator scenario runner

`uob-sim run` executes a versioned TOML scenario against separately configured stations and writes
only JSON Lines records to standard output. Human-readable diagnostics go to standard error. A
successful run exits with status 0; setup, assertion, timeout, and cancellation failures use
distinct nonzero statuses and include their category and stable failure code in the final JSONL
record.

Run the checked-in heartbeat example against a listening OCPP peer with:

```text
cargo run --package uob-sim -- run \
  --config bins/uob-sim/examples/simulator.toml \
  --scenario bins/uob-sim/examples/heartbeat.toml \
  --seed 42 \
  --format jsonl
```

Both documents currently require `schema_version = 1`. Configuration station IDs must be unique,
and each endpoint explicitly selects `1.6` or `2.0.1`. `station_capacity` bounds the complete run
(16 by default), while each station's `step_capacity`, `command_capacity`, and `trace_capacity`
bound queued scenario work, outstanding OCPP exchanges, and retained adapter traces. Exceeding a
configured bound fails explicitly; the final JSONL summary exposes rejected-step,
rejected-command, and dropped-trace counts.

OCPP 1.6 station topology uses `connectors = [1, 2]`. OCPP 2.0.1 topology uses one or more
`[[stations.evses]]` entries with an EVSE `id` and its `connectors`. When omitted, either version
defaults to its native resource numbered 1. Connector identities and transaction state are owned
by one station; a 1.6 connector is never collapsed into a 2.0.1 EVSE/connector pair.

Scenario steps name their station, action, and nonzero wall-clock timeout. Version 1 supports
`connect`, `boot`, `authorize`, `status`, `start_transaction`, `meter_values`,
`stop_transaction`, `await_remote_start`, `await_remote_stop`, `heartbeat`, `wait`, and
`disconnect`. Resilience scenarios additionally use `target_offline`, `target_online`, and
`reconcile_command`. Native OCPP 1.6 list/cache scenarios add `csms_offline`,
`csms_reconnect`, `offline_start`, `offline_stop`, `assert_local_authorization`,
`await_local_authorization`, `await_reboot`, `delay_local_reply` and `drop_local_reply`.
Each station executes its own ordered step queue,
so a delayed, disconnected, or missing-response station cannot stop another station from making
progress. Reports are reconstructed in source-step order to retain deterministic JSONL identifiers
even though station workers execute concurrently.

Charging actions carry an exact native JSON `payload`, an independently authored `fixture_id`, and
optional exact `expect_response`. The checked-in `charging-1.6.toml` sequence boots, authorizes,
reports connector state, starts and meters a transaction, reconnects without losing simulator-owned
state, then stops. The parallel `charging-2.0.1.toml` scenario uses native `TransactionEvent`
Started/Updated/Ended messages against a multi-EVSE topology and retains transaction ID, sequence,
EVSE/connector, phase, unit, context, location, and source timestamp. Duplicate, skipped, or
replayed sequence numbers, flattened EVSE identities, and incomplete meter-quality fields fail
before transmission. A rejected authorization does not make an identity eligible for a later start.

The checked-in `resilience-1.6.toml` and `resilience-2.0.1.toml` scenarios exercise missing,
denied, and expired local credentials while keeping each protocol's native token and resource
shape. `expect_failure` names the exact stable failure that is required for a step to pass; an
unexpected success or a different failure code fails the run. This lets a scenario continue after
proving that a denied start produced no transaction or physical effect.

Inbound OCPP 1.6 `RemoteStartTransaction`/`RemoteStopTransaction` and OCPP 2.0.1
`RequestStartTransaction`/`RequestStopTransaction` requests are placed on the bounded station
command queue. Their CALLRESULT acceptance is recorded independently from subsequent scenario
actions: an accepted remote start does not fabricate a started transaction, and an accepted remote
stop does not fabricate a stopped transaction. Unknown connectors or EVSEs and inactive transaction
identifiers are rejected. Scenario steps can consume these commands with
`await_remote_start` and `await_remote_stop`, including the original request payload and the
separate acceptance boolean.

For an OCPP 2.0.1 `start_transaction` step after `await_remote_start`, setting
`use_awaited_remote_start_id = true` injects the actual accepted request's
`remoteStartId` into the native `TransactionEvent` Start `transactionInfo`
correlation instead of using a hard-coded fixture ID. This is opt-in and
requires the station to have consumed an accepted remote-start command;
without such a command the run fails rather than inventing a successful
correlation. The [Compose demos](../testing/compose-profiles.md) exercise
this path with fresh per-run credentials and both OCPP editions.

Remote-command resilience steps can carry `request_id`, `delivery_id`, `execute_at_ms`, and an
optional `expires_at_ms`. A tracked command that has reached its deadline fails with
`command_expired` before the simulator consumes a charger command. Reusing either identity yields
`duplicate_suppressed` and cannot increment the station's physical-effect count. These logical
times are deterministic scenario evidence, not wall-clock or network time.

`target_offline` and `target_online` model availability of the selected external target without
disconnecting the charger. The resilience examples authorize and begin charging while that target
is offline, then report the unchanged physical-effect count when it reconnects. A
`missing_response` fault on a tracked accepted remote command records `transmission_uncertain`:
the charger may have acted, so the command is not rejected or replayed. `reconcile_command` can
confirm only that uncertain request from later observed state and reports
`confirmed_without_replay` with the same effect count.

`start_delay_ms` adds a station-local delay before an action; `jitter_ms` adds a deterministic
seed-derived value from zero through that bound. A heartbeat or compatible remote-command
await step can carry a `[steps.fault]` table with `kind`, `probability_percent`, and (where
required) `delay_ms`. Supported controls depend on the authored action:

- `disconnect`: close the selected station before its heartbeat;
- `response_delay`: for heartbeat, hold simulator-observed step completion **after** the
  Heartbeat exchange, without delaying a peer reply; for `await_remote_start` and
  `await_remote_stop`, actually delay the simulator's OCPP CALLRESULT to the peer;
- `missing_response`: for heartbeat, start the exchange but suppress its observed completion
  until its step timeout; for a tracked remote command, record a possible physical effect
  and require an explicit `transmission_uncertain` expectation;
- `out_of_order_response`: for heartbeat, issue a bounded pair of exchanges, hold the first
  completion, and allow the second correlated response to complete first; this requires
  `command_capacity` of at least two.

The opt-in [control API](control-api.md) exposes `response_delay_scope` as
`"step_completion"` for heartbeat and `"peer_reply"` for remote-command await steps.
Its `effect_status` reports an intervention's execution separately from a step's
assertion result; applied interventions can still fail assertions. Remote-command
response delays must be shorter than the step deadline.

Fault selection is deterministic for the run seed, station ID, and step ID, and a selected control
produces a `fault_selected` JSONL record. The heartbeat action can explicitly expect the
`Heartbeat` wire message and its resulting event; other actions can similarly name their expected
event. Unsupported versions, topologies, actions, messages, events, fault combinations, unknown
fields, and station references fail closed.

The scenario contains an explicit seed. `--seed` overrides it for a particular run.
TOML integer seeds fit its signed 64-bit range; for the remaining unsigned 64-bit
range use a quoted canonical decimal string, e.g. `seed = "18446744073709551615"`.
Control API run IDs and all seed values use exact decimal strings on the JSON wire.
The seed and a monotonic logical sequence produce stable event identifiers, so
the same validated scenario and seed have the same action/event order and IDs.
Reports deliberately contain no wall-clock report timestamps. A `wait` action uses the
injectable simulator clock in local tests, while every step, including real WebSocket
operations, retains an independent Tokio wall-clock timeout. Advancing a test clock
never advances bridge or network time.

Configuration can reference a credential file, but reports never serialize that path or the
endpoint. Parser and connection failures use redacted messages instead of echoing TOML source,
URLs, or dependency errors. On failure or Ctrl-C, the runner cancels every station worker,
force-closes in-flight connections, and drains the worker set before it returns.

## Native OCPP 1.6 local list, cache and real offline recovery

Configure a separate protected persistent model in the simulator document:

```toml
[[stations]]
id = "demo-1"
endpoint = "ws://127.0.0.1:9000/ocpp/demo-1"
ocpp_version = "1.6"
connectors = [1]

[stations.local_authorization]
private_state_file = "/srv/uob-demo/private/simulator/demo-1.json"
list_supported = true
cache_supported = true
```

The file's canonical directory must be owner-only mode `0700`; an existing file
must be service-owned mode `0600`. It must not alias credentials or another station.
Provision it separately from bridge state and protected bridge update content.
Missing model configuration is genuinely unsupported, not ephemeral Accepted.
Only OCPP 1.6 accepts this configuration. The authored
`bins/uob-sim/examples/local-authorization-1.6.toml` shows scenario action shapes;
the independent CSMS must actually send the native updates expected by the scenario.

Native Full replaces the list; absent/empty Full clears it. Differential upserts,
deletes entries without `idTagInfo`, and requires its version to exceed the stored
version; absent/empty Differential changes no entries. Query reports `0` for empty,
even after a versioned clear; `-1` means unsupported. ClearCache never clears the
list. List/cache lookup uses full Unicode casefold without changing transmitted
spelling or the service's independent exact-byte SHA policy. Negative/expired entries
deny local starts. An enabled list takes priority over the separate cache; a retained
disabled list does not suppress an enabled cache. Recovery preserves legitimate
overlap between the independent maps across support toggles, and re-enabling the
list restores its priority.

Unlike `target_offline`, `csms_offline` actually shuts down the station socket.
`offline_start` requires a native payload with `connectorId`, `idTag`, `meterStart`
and `timestamp`, with `expect_response = { accepted = true }` or false as appropriate.
A confirmed online transaction still occupies its connector after `csms_offline`;
`offline_start` rejects that connector before adding any durable record. A distinct
free connector remains usable, and a confirmed native online Stop frees its connector.
`offline_stop` requires `connectorId`, `meterStop`, `timestamp` and expects
`{ stopped = true }`. Native accepted facts are durably recorded without an Authorize
CALL or open socket. Denied starts create no transaction. Caps fail rather than
truncate or invent successfully persisted facts.

`csms_reconnect` establishes a new socket, obtains native Boot acceptance and performs
bounded Start/Stop replay with original tags, meters and timestamps and the returned
signed native transaction ID. It reports `registered_and_replayed` or honestly
`registered_replay_uncertain`; it does not retry an uncertain Start. Successful finished
records leave the queue only after native mapping/Stop and durable commit.

`assert_local_authorization` checks a requested subset of safe state metrics, and
`await_local_authorization` waits under the step deadline. Metrics include
`listVersion`, `listEntries`, `cacheEntries`, `offlineRecords`, `uncertainRecords`
and `stateAvailable`, never raw/parent identities. `await_reboot` observes an actual
native Reset lifecycle: Accepted reply, actual close, disk recovery, new socket,
Boot and bounded replay attempt. Configured successful recovery reports
`disk_recovered_and_registered`. An unconfigured Reset cannot claim disk recovery.
Reset ends ongoing offline records with the native Reset reason; absent an explicit
later stop measurement, their last available reading is the recorded start meter,
not invented energy growth.

`delay_local_reply` uses `duration_ms` to delay the next actual local-list/cache
CALLRESULT after durable mutation. `drop_local_reply` commits that mutation, drops
its acknowledgement and actually closes the socket. Both require a connected
native client; explicit reconnect/query and a fresh authorized Full can reconcile
state, while a version alone cannot establish list contents.

Public JSONL, adapter traces and the authenticated control catalog/progress expose
safe counters/statuses only. Raw tags/parents necessarily remain in the separate
owner-only recovery file and native traffic. Kill/new-process recovery and native
Reset are different exercised boundaries; neither implies physical charger behavior,
OCA certification or exactly-once network execution.

## Native OCPP 2.0.1 local list and authorization cache

An explicitly configured `[stations.local_authorization]` also enables the
separately typed native 2.0.1 model. Use the
[native configuration, semantics and actual joint smoke](local-authorization201.md)
and `bins/uob-sim/examples/local-authorization-2.0.1*.toml`. Do not reuse a 1.6
private state file, signed list version, tag shape or returned numeric transaction ID.

The existing local actions dispatch by edition. Native `offline_start`/`offline_stop`
take original Started/Ended TransactionEvent payloads, with native transactionId,
EVSE/connector, seqNo, timestamp and typed IdToken. Safe assertions accept only
availability and list/cache/offline/uncertainty counters. Delayed/dropped management
replies occur after durable native mutation. Real reconnection/Reset requires a fresh
current-socket Accepted Boot before delivering original pending facts; a killed
Sending record becomes Uncertain and is not automatically retried.

Full omission and Differential omission differ; explicit empty arrays remain invalid.
Full entries require information; Differential omission of entry information deletes
the typed identity. Empty installed lists keep a positive version, whereas a disabled
or uninitialized native list queries zero. Actual native CSMS information refreshes
cache entries even when denied or list-known; enabled list membership still has
priority. Cache-only clearing cannot grant or revoke separate central policy.

