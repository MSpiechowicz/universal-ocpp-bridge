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
`disconnect`. Each station executes its own ordered step queue,
so a delayed, disconnected, or missing-response station cannot stop another station from making
progress. Reports are reconstructed in source-step order to retain deterministic JSONL identifiers
even though station workers execute concurrently.

The OCPP 1.6 charging actions carry an exact native JSON `payload`, an independently authored
`fixture_id`, and optional exact `expect_response`. The checked-in
`bins/uob-sim/examples/charging-1.6.toml` sequence boots, authorizes, reports connector state,
starts and meters a transaction, disconnects and reconnects without losing simulator-owned
transaction state, then stops. Meter readings remain strings with their source timestamp,
measurand, context, format, location, and unit. A rejected authorization does not make the tag
eligible for a later start.

Inbound `RemoteStartTransaction` and `RemoteStopTransaction` requests are placed on the bounded
station command queue. Their CALLRESULT acceptance is recorded independently from subsequent
scenario actions: an accepted remote start does not fabricate a started transaction, and an
accepted remote stop does not fabricate a stopped transaction. Unknown connectors and inactive
transaction identifiers are rejected. Scenario steps can consume these commands with
`await_remote_start` and `await_remote_stop`, including the original request payload and the
separate acceptance boolean.

`start_delay_ms` adds a station-local delay before an action; `jitter_ms` adds a deterministic
seed-derived value from zero through that bound. A heartbeat can carry a `[steps.fault]` table with
`kind`, `probability_percent`, and (where required) `delay_ms`. Supported controls are:

- `disconnect`: close the selected station before its heartbeat;
- `response_delay`: hold the completed response for the configured delay;
- `missing_response`: start the exchange but suppress its observed completion until its step
  timeout;
- `out_of_order_response`: issue a bounded pair of exchanges, hold the first completion, and allow
  the second correlated response to complete first; this requires `command_capacity` of at least
  two.

Fault selection is deterministic for the run seed, station ID, and step ID, and a selected control
produces a `fault_selected` JSONL record. The heartbeat action can explicitly expect the
`Heartbeat` wire message and its resulting event; other actions can similarly name their expected
event. Unsupported versions, topologies, actions, messages, events, fault combinations, unknown
fields, and station references fail closed.

The scenario contains an explicit seed. `--seed` overrides it for a particular run. The seed and a
monotonic logical sequence produce stable event identifiers, so the same validated scenario and
seed have the same action/event order and IDs. Reports deliberately contain no wall-clock report
timestamps. A `wait` action uses the injectable simulator clock in local tests, while every step,
including real WebSocket operations, retains an independent Tokio wall-clock timeout. Advancing a
test clock never advances bridge or network time.

Configuration can reference a credential file, but reports never serialize that path or the
endpoint. Parser and connection failures use redacted messages instead of echoing TOML source,
URLs, or dependency errors. On failure or Ctrl-C, the runner cancels every station worker,
force-closes in-flight connections, and drains the worker set before it returns.
