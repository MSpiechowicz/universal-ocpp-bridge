# EMS/SCADA OpenAPI contract

The selected direct HTTP target publishes `GET /bridge/v1/openapi.json` as OpenAPI 3.1.1,
with contract version 1.0.0. The checked-in artifact is generated from the integration response
models, query parameters, error mapping and resource inventory. The listener serves that exact
artifact without per-request schema generation.

Canonical objects reference each contract's current versioned schema on the same listener:
command-result uses `/bridge/v1/schemas/v1.10/command-result.schema.json`, and nested
export-record/export-batch use `/bridge/v1/schemas/v1.11/{schema}`. Other unchanged
contracts retain their existing versioned paths. Responses contain the unmodified files
from `crates/contracts/schemas`, including their canonical `$id` and Draft 2020-12
definitions. Released command-result schemas through v1.9 and export schemas through
v1.10 remain available byte-identical; old result JSON remains readable.

The initial protected request envelopes are served at
`/bridge/v1/schemas/v1.0/set-variables-reference-201.schema.json` and
`/bridge/v1/schemas/v1.0/set-network-profile-reference-201.schema.json`. Their payload
identities are `urn:uob:ocpp201:SetVariablesReference:1` and
`urn:uob:ocpp201:SetNetworkProfileReference:1`, distinct from native OCA request URNs.
They describe reference-only inputs, not secret-bearing native network profiles.
Schema discovery neither grants privileged target ingress nor enables a protected-value
provider or automatic export production.

The protected OCPP 1.6 list envelope is served at
`/bridge/v1/schemas/v1.0/send-local-list-reference-16.schema.json`, payload identity
`urn:uob:ocpp16:SendLocalListReference:1`. It carries version, update type and a
station-bound opaque capability, never inline native entries or parent identities.

The protected 201 envelope uses
`/bridge/v1/schemas/v1.0/send-local-list-reference-201.schema.json`,
`urn:uob:ocpp201:SendLocalListReference:1`, positive versionNumber/updateType and
an exact station-bound list201 capability. Optional result v1.10
`local_authorization_201` contains only nonnegative query version, requested Send
version/type/status, or Clear status. Native statusInfo, entries, token/group metadata,
personal messages and capabilities are omitted. Public nested export v1.11 and
separately named runtime revision10 retain stored historical record versions.

Clients can resolve the contract without contacting a schema registry. Configure
resolvers with the document's retrieval URL as their base and supply the integration
bearer token for same-origin schema requests. The contract probe preloads these files
into an offline registry and never forwards credentials to another origin.

The document covers capabilities, station inventory/snapshots, point pages/values, command
admission/status, SSE, schemas, and the document itself, including Axum's implicit HEAD methods.
It describes pagination, runtime bounds, resource-scoped reader/control permissions, the
credential-free loopback exception for contract discovery, stable errors, typed command rejection
responses, and `WWW-Authenticate`. Malformed station/point path encoding returns the same stable
`ems_scada_http.invalid_request` object as malformed query parameters.

An external client initiates HTTPS requests and HTTP SSE; there are no webhooks or broker
requirements. The direct target's acknowledgement means local exposure, not EMS consumption.
A command's 202 response means durable admission or replay; protocol acceptance and independently
observed effects remain separate fields in its canonical result. SSE `durable` records contain an
EventEnvelope and cursor; `gap` and `error` are terminal control records without durable IDs.
Telemetry is best effort and outside this durable stream. Full transport semantics are embedded
in the document from `adapters/target-ems-scada-http/openapi/semantics.md`.

Optional command-result v1.8 `configuration_201` preserves value-free native write
acknowledgements. SetVariables keeps exact component/variable/attribute identities and
all six native statuses: `Accepted`, `Rejected`, `UnknownComponent`, `UnknownVariable`,
`NotSupportedAttributeType` and `RebootRequired`. A mixed result preserves every item
with aggregate acceptance false; all Accepted/RebootRequired items aggregate positively
without proving an observed effect. SetNetworkProfile keeps the signed 32-bit slot
(zero is valid), exact `Accepted`/`Rejected`/`Failed`, and `staged`; Accepted means stored
for activation after a separate operator-controlled reboot, not already active connectivity.
Results contain no values, profiles, `statusInfo`, `customData` or reusable capabilities.
Native acceptance, reboot staging, HTTP exposure and independently observed effects
remain separate facts.

The configuration-evidence addition used public nested export v1.9 and separately
named runtime envelope revision 8. The 1.6 list addition used public v1.10/runtime
revision9; native 201 now uses public v1.11/runtime revision10, without a SQL migration.
The existing exact-origin/principal/resource status checks, configured authentication
policy and encoded-payload caps are unchanged. Ordinary target credentials cannot submit
these privileged writes, and oversized results fail explicitly without truncated success.

Optional command-result v1.9 `local_authorization_16` preserves native OCPP 1.6
Get version, Send requested version/update type and exact native status, or Clear
status. Query empty `0` and unsupported `-1` remain distinct; Send forbids the
update sentinels but preserves other signed 32-bit versions. No entries, parents,
capabilities or arbitrary native errors appear in results. ACK is not installed
contents, offline use or physical effect proof; malformed/lost/unpaired replies
have no fabricated local-list field. Result retrieval remains exact-origin,
principal and resource scoped; this schema addition does not permit privileged
OCPP target ingress or automatic global export production.

## Separate MQTT transport

The optional `ems-scada` preset on the MQTT target exposes the same canonical point descriptors
and values as retained MQTT topics, alongside station state, durable events and explicit command
results. It is not an OpenAPI endpoint and does not convert a vendor HTTP API to MQTT. For the
independent MQTT 3.1.1/TLS consumer, scoped ACL fixture and opt-in two-protocol broker acceptance
command, see [MQTT target: Independent EMS/SCADA MQTT acceptance](../configuration/mqtt-target.md#independent-emsscada-mqtt-acceptance).
The direct HTTP/SSE listener and its broker-free acceptance below remain unchanged; selecting
one target does not implicitly start the other. Neither target's transport acknowledgement proves
EMS consumption or electrical energy delivery.

MQTT explicitly accepts named command-result v1.0/v1.1/v1.4/v1.5/v1.6/v1.7/v1.8/v1.9/v1.10
and runtime revisions 11 (OCPP 1.6 reservations), 12 (OCPP 2.0.1 reservations), 13
(OCPP 2.0.1 composite schedules and installed-profile reports), 14 (OCPP 1.6 firmware
updates), 15 (OCPP 2.0.1 firmware updates) and 16 (OCPP 1.6 diagnostics and log uploads)
on existing topics. Historically unsupported v1.2/v1.3 remain unsupported, and future revision
17 is rejected before publication. Immediate and durable results retain
packet-specific PUBACK correlation and existing encoded-message caps; oversized
results fail without truncated evidence. Broker receipt is not native acceptance,
installed list contents, offline authorization or physical effects. Result compatibility
grants no native target privilege.

## Regeneration and CI drift gate

```text
cargo run --locked -p uob-contracts --example export_public_schemas -- crates/contracts/schemas
cargo run --locked --quiet -p uob-ems-scada-http-target-adapter \
  --example export_openapi > /tmp/uob-openapi.json
cp /tmp/uob-openapi.json adapters/target-ems-scada-http/openapi/v1.json
cargo test --locked -p uob-ems-scada-http-target-adapter
./scripts/verify-workspace.sh
```

Generate into a temporary file first so a compilation failure cannot truncate the published
artifact. Schema/model or query changes fail the generated-versus-published snapshot test. Route
inventory is also checked against the actual router. The existing read, command and SSE scenarios
validate actual response bodies and stream payloads against the document. Negative checks cover
stale route/model snapshots, unknown command kinds and injected origins.

OpenAPI structure is validated offline against the official schema fetched from
[OpenAPI 3.1 schema 2025-09-15](https://spec.openapis.org/oas/3.1/schema/2025-09-15).
The pinned copy is `adapters/target-ems-scada-http/openapi/oas-3.1-schema-2025-09-15.json`,
SHA-256 `d0a3955182364c7b5fdebfd0583ecad259a870b4a2fe86a1b0fe8785f8224fed`.
It validates the OpenAPI document structure; the separate Rust JSON Schema validator compiles
every response/component reference and validates examples and observed payloads. CI already runs
these tests through the mandatory workspace verifier; no network validation or new Node toolchain
is required. The official schema is third-party material from the
[Apache-2.0-licensed OpenAPI Specification](https://github.com/OAI/OpenAPI-Specification/blob/main/LICENSE).

## Broker-free runnable contract demo

```text
cargo test --locked -p uob-ems-scada-http-target-adapter \
  broker_free_contract_demo_validates_both_ocpp_resource_scenarios -- --nocapture
```

This self-contained demo starts the real authenticated Axum integration router on an ephemeral
loopback socket, supplies canonical fixture state for OCPP 1.6 connector and OCPP 2.0.1 EVSE/connector
scenarios, and runs the simulation-owned HTTP probe from `tests/ems-http-contract-client/probe.rs`.
The complete call definition is `tests/ems-http-contract-client/demo.toml`. Eight requests are
selected by OpenAPI operation ID and checked against the fetched contract; each scenario must
expose the expected native protocol identity. The probe imports no bridge application/domain
models, protocol handlers, or persistence code. No MQTT adapter or broker is started.

The same probe can run against an independently started integration listener by supplying a
scenario TOML with its station/resource identifiers and a read-scoped token in `UOB_EMS_TOKEN`:

```text
cargo run --locked -p uob-ems-scada-http-target-adapter --example probe_http_contract -- \
  https://ems.example:9080 tests/ems-http-contract-client/demo.toml
```

The external client requires HTTPS for non-loopback API bases; plaintext HTTP is accepted only
for IPv4/IPv6 loopback or exact `localhost` fixtures. The probe has a five-second per-request
deadline and a 45-second overall deadline, a 1 MiB per-response bound, at most 32 advertised
schemas and 1 MiB total schema response bytes. Redirects are disabled. It validates the
published OpenAPI document and same-origin canonical schema references. The fixture-only
contract demo does not claim to drive chargers.

An empty read-only scenario set fails locally before any API request; nonempty sets may cover
one protocol. Active-exercise protocol requirements are separate.

## Independent HTTP/SSE EMS acceptance

Build the external executable, then run the test-only loopback host and public `uob-sim` scenario
runner against it:

```text
cargo build --locked -p uob-ems-scada-http-target-adapter --example probe_http_contract
UOB_EXAMPLE_PATH="$PWD/target/debug/examples/probe_http_contract" \
  cargo test --locked -p uob-ems-scada-http-target-adapter \
  --test issue87_integration -- --nocapture
```

The executable's `--exercise` mode accepts an API base URL and
`tests/ems-http-contract-client/demo.toml`; reader and operator bearer tokens come from
`UOB_EMS_TOKEN` and `UOB_EMS_OPERATOR_TOKEN`. It emits one JSON result with separate HTTP
admission, protocol response, and observed transaction-transition evidence for OCPP 1.6J
and 2.0.1. The test-only host joins the actual EMS target, application command coordinator,
authenticated OCPP endpoint, operational SQLite journal/outbox and simulator WebSocket flows;
production service startup and the public API remain unchanged. No MQTT adapter or broker runs.

Loopback active exercises use `--exercise` without further options. A non-loopback HTTPS
active exercise requires the deliberate `--allow-remote-exercise` option as well; the client
rejects remote exercises without it before any API request. This option is invalid without
`--exercise`, and remote plaintext remains unsupported. The read-only contract probe above
continues to support remote HTTPS without an active-exercise opt-in.

```text
cargo run --locked -p uob-ems-scada-http-target-adapter --example probe_http_contract -- \
  https://ems.example:9080 tests/ems-http-contract-client/demo.toml \
  --exercise --allow-remote-exercise
```

Only run an active exercise against an intended test EMS: it submits real start/stop
commands. An exercise result reports `status: passed` only after completing command
scenarios for both `ocpp16` and `ocpp201`; a scenario missing either commandable
protocol fails before any HTTP request rather than claiming partial acceptance.

The scenario path must be a regular file of at most 64 KiB; reads stop at 64 KiB + 1 bytes to
detect a file that grows after its metadata check.

The client walks paginated station/point inventories (requiring more than one `limit=2` point
page per scenario), reads a point value, verifies reader denial and station scope, exercises
expired/conflicting/duplicate request IDs, resumes durable
SSE IDs after reconnect, and recovers an expired cursor by fetching the fresh station snapshot
before subscribing without the stale cursor. A terminal gap also requires this recovery; a
control record's `id` is never a checkpoint. One charging flow has no subscriber until
completion; the other holds an unread station-level SSE connection during command and
transaction processing. The replay/correlation subscription uses the observed transaction resource.
All requests and stream frames are bounded, redirects are disabled, and status/recovery links
must remain same-origin and free of URL credentials. Diagnostics omit bearer tokens.

The test-only acceptance fixture sequences a charger Heartbeat after each start
transaction and before testing remote stop, establishing charger-side readiness
for both protocols. The external client still independently checks HTTP admission,
protocol acceptance, and observed transaction effects.

The test asserts both simulator-side remote-command acceptance and later native transaction
start/stop observations. `202` is only durable admission; a pending transaction reports station
observation, not proof of electrical power flow. Independently, committed outbox deliveries
are reported as local `/bridge/v1` exposure, never as EMS peer consumption.
