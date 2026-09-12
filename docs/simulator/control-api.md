# Opt-in simulator control API

`uob-sim serve --config control.toml --control-bind 127.0.0.1:9001` exposes a
separate, authenticated loopback API. `run` keeps its existing behavior and never
starts a control listener. Simulator code and controls are excluded from the production daemon.

The control configuration is deliberately separate from the regular simulator configuration:

```toml
schema_version = 1
environment = "demo"
token_file = "simulator-control.token"
simulator_file = "simulator.toml"

[[scenarios]]
id = "heartbeat"
path = "heartbeat.toml"
```

Paths are relative to the control document. Provision a separate control token containing
32 cryptographically random bytes encoded as 64 hexadecimal characters, with access restricted
to the test operator. Neither browser cookies nor bridge management credentials authenticate
this listener. All requests, including catalog and status reads, require
`Authorization: Bearer <token>`. Responses use `Cache-Control: no-store`.

Only explicit `demo` and `staging` environments are accepted. Station IDs must start with
`demo-` or `staging-` respectively. Each station's `ws://` or `wss://` endpoint must use a
literal loopback IP and end in that exact station ID. Hostnames, URL credentials, query
strings, fragments, credential files and automatic reconnect are rejected. Use separately
provisioned test peers that accept these synthetic identities; never configure a production
listener as a test peer. These startup declarations are trusted operator configuration,
not remote attestation of the peer. Production configuration is not a supported input.

The HTTP Host must exactly match the configured IP and port, including IPv6 brackets.
A supplied Origin must be that listener's HTTP origin. Cross-origin requests and ambient
cookie authentication are rejected. An authorized future console integration can use a
same-origin server proxy that explicitly supplies the separate simulator token; it must not
forward bridge credentials or expose the token to arbitrary origins.

## Endpoints

| Method and path | Request / result |
| --- | --- |
| `GET /api/v1/scenarios` | Environment and configured scenario IDs |
| `GET /api/v1/runs` | Bounded run IDs, scenario IDs, seeds and terminal flags; recovers a lost start response |
| `POST /api/v1/runs` | `{"scenario":"heartbeat","seed":42}`; seed optional; returns `run_id` |
| `GET /api/v1/runs/{id}` | Environment, scenario, seed, status, per-step progress and final logical events |
| `POST /api/v1/runs/{id}/stop` | Cancels only that run; status becomes terminal after socket cleanup |
| `POST /api/v1/runs/{id}/controls` | Schedules one validated intervention on a pending step |
| `DELETE /api/v1/runs/{id}` | Removes a terminal result to reclaim capacity; active runs cannot be deleted |

There is no charging-command endpoint. Browser start/stop charging commands still go through
the bridge's ordinary authenticated command API. Starting an allowlisted scenario drives the
same real charger-side OCPP exchanges as `run`, including separately observed remote-command
acceptance. HTTP clients cannot upload arbitrary scenario definitions, station endpoints or
native charger commands.

For a pending heartbeat, inject a delayed response with:

```json
{"step_id":"heartbeat","intervention":{"kind":"fault","fault":"response_delay","delay_ms":100}}
```

The other native runner fault types are `disconnect`, `missing_response` and
`out_of_order_response`. Existing action/fault compatibility checks apply. Injection sets
selection probability to 100%; the existing step deadline remains authoritative. A delayed
response holds the simulator's observed completion, as in `run`; it does not delay the bridge's
clock or rewrite a response on the wire.

Disconnect/reconnect controls replace explicitly authored, pending `wait` checkpoints:

```json
{"step_id":"disconnect-checkpoint","intervention":{"kind":"disconnect"}}
```

Use `{"kind":"reconnect"}` at a later wait checkpoint to execute the normal `connect`
action with the station's existing simulator state. Provide an earlier wait window when an
operator needs time to schedule interventions. Controls are scheduled at step boundaries,
not advertised as immediate socket changes. Already-running, completed, unknown and already
edited steps fail with a conflict. Edits and worker admission share a lock, preventing a late
control from silently changing another step. Every applied edit remains visible in step metadata.

## Bounds and evidence

Startup documents and request bodies are limited to 64 KiB; the token file to 128 bytes.
The catalog retains at most 16 scenarios and the server at most eight active or completed runs.
Active runs cannot share a station. The configured simulator has at most 16 stations,
256 steps per scenario, 2–16 outstanding station commands and at most 128 trace records per
station. Steps, delays and request timeouts are capped at 30 seconds, and summed step timeouts
at 120 seconds per scenario. Normal runner cleanup adds at most four seconds per station,
with station workers cleaning up concurrently. Request handling admits at most 16 concurrent
authenticated requests and imposes a five-second body/handler deadline. Excess requests or
run capacity fail explicitly. Remove terminal results before admitting further runs.

Status reports retain step identity, action, declared event expectation, assertion outcome,
fault and intervention metadata. They also return final logical event IDs, seeds, sequencing,
status and safe failure codes. Wire payloads and response details are omitted. Final logical
events preserve source-step ordering; live step status shows actual worker progress. Steps
not reached before a failed or cancelled run remain pending and are no longer editable.

Ctrl-C stops ingress, cancels the tracked runs and waits for normal bounded socket cleanup.
Stopping one run does not cancel other runs or stations owned by another run. Tests cover
both OCPP editions against real WebSocket peers, parity with the JSONL CLI, actual HTTP
startup, authentication/origin checks, payload and retained-result limits, scoped cancellation,
checkpoint reconnects, delayed responses and missing-response deadlines.
