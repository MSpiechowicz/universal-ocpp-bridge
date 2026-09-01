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
and each endpoint explicitly selects `1.6` or `2.0.1`. Scenario steps name their station, action,
and nonzero wall-clock timeout. Version 1 supports `connect`, `heartbeat`, `wait`, and `disconnect`.
The heartbeat action can explicitly expect the `Heartbeat` wire message and its resulting event;
other actions can similarly name their expected event. Unsupported versions, actions, messages,
events, unknown fields, and station references fail closed.

The scenario contains an explicit seed. `--seed` overrides it for a particular run. The seed and a
monotonic logical sequence produce stable event identifiers, so the same validated scenario and
seed have the same action/event order and IDs. Reports deliberately contain no wall-clock report
timestamps. A `wait` action uses the injectable simulator clock in local tests, while every step,
including real WebSocket operations, retains an independent Tokio wall-clock timeout. Advancing a
test clock never advances bridge or network time.

Configuration can reference a credential file, but reports never serialize that path or the
endpoint. Parser and connection failures use redacted messages instead of echoing TOML source,
URLs, or dependency errors. On failure or Ctrl-C, the runner stops all connected clients before it
returns.
