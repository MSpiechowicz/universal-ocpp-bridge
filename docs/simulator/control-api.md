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
to the test operator. Neither browser cookies, the bridge management credential, nor the
separate `[debug]` read credential authenticate control routes. All non-preflight requests,
including catalog and status reads, require `Authorization: Bearer <control-token>`.
Responses use `Cache-Control: no-store`. Browser access is disabled by default; to permit one
demo/staging console, add a separate block to this same control document:

```toml
[control_browser]
console_origin = "http://127.0.0.1:8080"
```

Only a canonical literal-loopback HTTP origin (`127.0.0.1` or `[::1]`) is accepted here.
It must identify the intended test bridge console. The `[debug]` block independently
specifies its own console origin and read token; neither opt-in enables the other.

Only explicit `demo` and `staging` environments are accepted. Station IDs must start with
`demo-` or `staging-` respectively. Each station's `ws://` or `wss://` endpoint must use a
literal loopback IP and end in that exact station ID. Hostnames, URL credentials, query
strings, fragments, credential files and automatic reconnect are rejected. Use separately
provisioned test peers that accept these synthetic identities; never configure a production
listener as a test peer. These startup declarations are trusted operator configuration,
not remote attestation of the peer. Production configuration is not a supported input.

The HTTP Host must exactly match the configured IP and port, including IPv6 brackets.
Without `[control_browser]`, a supplied Origin is accepted only if it is the control
listener's own HTTP origin; a headless same-listener client still needs the control bearer.
With `[control_browser]`, that one configured cross-origin console is also accepted.
Unconfigured cross-origin requests and cookies are denied. Browser CORS preflight is
permitted only for known control routes and their supported methods, requesting
`Authorization` (optionally `Content-Type`); actual requests still require the bearer.
CORS echoes only the configured origin and allowed methods/headers, never wildcard origins
or credentialed cookies. The [Debug evidence endpoint](../operations/debug-simulator-evidence.md)
has a separate read token and origin permission; it cannot authorize control routes.

## Endpoints

| Method and path | Request / result |
| --- | --- |
| `GET /api/v1/scenarios` | Environment and server-derived catalog: scenario IDs, exact decimal-string default seeds, complete authored station sets, and step IDs/stations/actions with eligible controls and `response_delay_scope` |
| `GET /api/v1/runs` | Bounded run IDs, scenario IDs, exact decimal-string seeds and terminal flags; recover a lost start response |
| `POST /api/v1/runs` | `{"scenario":"heartbeat","seed":"42"}`; optional canonical unsigned decimal-string seed override; returns decimal-string `run_id` |
| `GET /api/v1/runs/{id}` | Environment, scenario, exact decimal-string seed, status, per-step progress and final logical events |
| `POST /api/v1/runs/{id}/stop` | Cancels only that run; status becomes terminal after socket cleanup |
| `POST /api/v1/runs/{id}/controls` | Schedules one validated intervention on a pending step |
| `DELETE /api/v1/runs/{id}` | Removes a terminal result to reclaim capacity; active runs cannot be deleted |

Run IDs in paths, responses and requests are canonical unsigned decimal strings, not JSON
numbers. Seeds use the same exact decimal-string contract on the wire, including defaults,
overrides, run lists and status. Omit `seed` to use the authored scenario default; numeric
JSON seeds are rejected. TOML allows nonnegative integer seeds in its signed 64-bit range;
for the full unsigned 64-bit range, write a quoted canonical decimal seed such as
`seed = "18446744073709551615"`. Do not round these identifiers through JavaScript numbers.

There is no charging-command endpoint. Browser start/stop charging commands still go through
the bridge's ordinary authenticated command API. Starting an allowlisted scenario drives the
same real charger-side OCPP exchanges as `run`, including separately observed remote-command
acceptance. HTTP clients cannot upload arbitrary scenario definitions, station endpoints or
native charger commands.

For a pending heartbeat, schedule a simulator-observed completion delay with:

```json
{"step_id":"heartbeat","intervention":{"kind":"fault","fault":"response_delay","delay_ms":100}}
```

The other eligible native runner fault types depend on the authored action. The catalog's
`eligible_controls` is authoritative: `wait` permits `disconnect`/`reconnect`, heartbeat
permits `response_delay`/`missing_response`/`out_of_order_response`/`disconnect`, and
remote-start/stop await steps permit `response_delay` (also `missing_response` for tracked
requests). Existing action/fault compatibility checks apply. Injection sets selection
probability to 100%; the existing step deadline remains authoritative. For a pending
heartbeat, `{"kind":"fault","fault":"disconnect","delay_ms":0}` selects a fault that
tears down the station socket when applied and may cause the heartbeat step to fail.
This is distinct from the `{"kind":"disconnect"}` intervention at an authored `wait`
checkpoint. On heartbeat, `response_delay` holds **simulator step completion after the
Heartbeat exchange**, not a reply sent by the simulator to the peer; `missing_response`
likewise suppresses observed step completion. On `await_remote_start`/`await_remote_stop`,
`response_delay_scope = "peer_reply"` means the simulator actually delays the OCPP
CALLRESULT to its independent peer. The catalog and status label heartbeat timing
`"step_completion"` versus remote-command `"peer_reply"`. Neither mode advances the
bridge's clock.

The distinct disconnect/reconnect intervention kinds replace explicitly authored,
pending `wait` checkpoints:

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
fault and intervention metadata. `effect_status` distinguishes `scheduled`, `in_progress`,
`applied`, `not_selected`, `not_observed`, `failed` and `not_applied` interventions;
it is independent of `assertion_passed`, which can fail after a control took effect.
They also return final logical event IDs, exact decimal-string seeds, sequencing,
status and safe failure category/code (including setup failure). Wire payloads
and response details are omitted.
Final logical events preserve source-step ordering; live step status shows actual worker
progress. Steps not reached before a failed or cancelled run remain pending and are no
longer editable. A scheduled edit is not proof of wire effect or scenario success.

Ctrl-C stops ingress, cancels the tracked runs and waits for normal bounded socket cleanup.
Stopping one run does not cancel other runs or stations owned by another run. Tests cover
both OCPP editions against real WebSocket peers, parity with the JSONL CLI, actual HTTP
startup, authentication/origin checks, payload and retained-result limits, scoped cancellation,
checkpoint reconnects, delayed responses and missing-response deadlines.
