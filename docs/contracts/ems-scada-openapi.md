# EMS/SCADA OpenAPI contract

The selected direct HTTP target publishes `GET /bridge/v1/openapi.json` as OpenAPI 3.1.1,
with contract version 1.0.0. The checked-in artifact is generated from the integration response
models, query parameters, error mapping and resource inventory. The listener serves that exact
artifact without per-request schema generation.

Canonical objects use references to `/bridge/v1/schemas/v1.0/{schema}` on the same listener.
These responses contain the unmodified files in `crates/contracts/schemas/v1.0`, including their
canonical `$id` and Draft 2020-12 definitions. Clients can resolve the contract without contacting
a schema registry. Configure resolvers with the document's retrieval URL as their base and supply
the integration bearer token for same-origin schema requests. The contract probe preloads these
files into an offline registry and never forwards credentials to another origin.

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

## Regeneration and CI drift gate

```text
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

The probe has a five-second request deadline, a 1 MiB response bound and disabled redirects.
The self-contained demo verifies the HTTP contract using canonical fixture state; it does not
claim to drive chargers. The broader independent HTTP/SSE EMS client and charger-driven end-to-end
acceptance matrix remain their own planned items. Existing adapter tests separately exercise
real command admission, restart/status replay, SSE reconnect, expiry and bounded slow readers.
