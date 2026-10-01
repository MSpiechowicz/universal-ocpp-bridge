# Public JSON contract versioning

The canonical HTTP, MQTT, and external-export representations use JSON Schema Draft 2020-12.
The published v1.0 snapshots live in `crates/contracts/schemas/v1.0`; each schema has an
independent `$id` that forthcoming OpenAPI documents can reference without making an HTTP or
MQTT transport authoritative for the underlying contract.

`ContractVersion.major` identifies semantic compatibility. `revision` identifies an additive
revision within that major. A v1 revision may add an optional response, event, snapshot, or
diagnostic field. Readers must ignore optional fields they do not understand. A v1 revision must
not remove or rename a field or variant, change a field type or meaning, narrow an accepted enum,
or make a previously optional field required. Those changes require a new major version and a new
schema directory.

Command ingress is deliberately stricter. Unknown command operations fail decoding, and a known
operation is rejected unless the addressed resource explicitly advertises its exact capability.
Privileged OCPP payloads additionally name their separately pinned payload schema. Unknown fields
must never be interpreted as a command or capability request.

`TraceRecord` is a best-effort diagnostic schema. It carries process and trace sequencing,
correlation, stage, direction, duration, target identity, outcome, and already-redacted bounded
details. It is intentionally separate from `EventEnvelope`: trace capture does not inherit durable
business-event retention, replay, ordering, or audit promises.

The contracts tests validate every canonical fixture against its published schema and compare the
checked-in files with schemas generated from the Rust types. The compatibility checker exercises
property removal, type changes, newly required fields, and narrowed semantic enums. When adding an
additive v1 revision, retain the prior snapshot and run the same checker from the earlier schema to
the new one; never rewrite a released snapshot.

Regenerate snapshots from the repository root with:

```text
cargo run --package uob-contracts --example export_public_schemas -- crates/contracts/schemas
```

The optional OCPP 2.0.1 `remote_start_id` in transaction protocol evidence is published in
v1.1 station-snapshot, export-record and export-batch schemas. Their v1.0 snapshots remain
unchanged and compatibility tests compare the revisions. The generator takes the schema root
and writes each contract to its current revision directory.

OCPP 1.6 configuration evidence adds optional `configuration` and
`configuration_observations` fields to the command result at v1.1. Because export
record and export batch embed that result, their additive snapshots advance to
v1.2 while v1.0/v1.1 exports remain unchanged. The bridge-owned
`configuration-change-reference` v1.0 schema accepts only a key and an opaque
protected reference; it is not the native OCA ChangeConfiguration request.

The EMS HTTP contract retains command-result schema routes v1.0–v1.4 and serves
the current v1.5 command-result; OpenAPI command result responses reference v1.5.
Current nested export-record and export-batch references use v1.6, while historical
schema routes and released snapshots remain available.

OCPP 1.6 `TriggerMessage` adds optional `trigger_observation` to command-result
v1.2, with immutable requested class/native scope/expected targets, dispatch
start and 60-second deadline, optional exact native reply and bounded
compatible-event IDs. Its `pending`, `partial`, `observed`, `absent` and
`unsupported` states describe compatible later messages, not causal proof or
physical effects. Nested export-record and export-batch schemas advance to
v1.3; earlier command-result v1.0/v1.1 and export v1.0/v1.1/v1.2 snapshots
remain unchanged. Older v1 readers may ignore the additive optional field.

OCPP 2.0.1 `TriggerMessage` adds a **separate**, optional
`trigger_observation_201` to command-result v1.3; it does not reuse the 1.6
connector-target field. The native EVSE/connector scope and frozen targets,
60-second dispatch window, exact optional native `Accepted`/`Rejected`/
`NotImplemented` reply with `statusInfo`, compatible event IDs and independent
`pending`/`partial`/`observed`/`absent`/`unsupported`/`unattributable`
observation status are additive response evidence, never causal or physical
charging proof. Nested export-record and export-batch schemas advance to v1.4;
previous command-result v1.0–v1.2 and export v1.0–v1.3 snapshots remain
unchanged. Older compatible v1 readers may ignore these optional fields. This
contract addition and SQLite schema v11 do not implement the separately
planned certificate-chain, CSR issuance or ISO 15118 workflows.

OCPP 1.6J `GetCompositeSchedule` adds optional `composite_schedule_16` to
command-result v1.4. Typed evidence contains immutable connector/duration/optional-unit
request context, exact native `Accepted`/`Rejected`, supplied timestamps and connector
identity, and optional native schedule metadata. Rates/minimum rates are exact canonical
decimal strings; genuine zero is not absent or Rejected. Accepted evidence requires a
meaningful complete schedule. Malformed or semantically invalid native CALLRESULT stays
`transmission_uncertain` without that evidence; a valid CALLERROR uses existing sanitized
rejection. Older result JSON decodes with the optional field absent.

Nested export-record and export-batch advance to v1.5 because they embed CommandResult.
Released command-result v1.0–v1.3 and export v1.0–v1.4 snapshots are retained unchanged.
These additive response versions do not relax strict privileged command ingress, implement
OCPP 2.0.1 schedules, or establish local profile installation/enforcement or physical effects.

OCPP 2.0.1 read-only GetVariables/GetBaseReport/GetReport add optional typed
`device_model_201` evidence to command-result v1.5. It freezes query identities,
selectors/criteria, native signed 32-bit report request ID and connection/generation.
GetVariables keeps independently matched native statuses, original casing and
absent/empty/redacted value facts. Reports keep native ACK separately from
`pending`, `complete`, `incomplete` or `not_expected`; only complete evidence
contains ordered sanitized inventory and fragment metadata. Incomplete evidence
contains a precise reason and optional accepted counts; unavailable recovered
counts remain unknown, not zero. Fragments without Accepted ACK cannot establish
completion, and wire NotifyReport acknowledgement is not durable completion.

Nested export-record and export-batch advance to v1.6 because they embed that
result. Released command-result v1.0–v1.4 and export v1.0–v1.5 snapshots remain
unchanged; older compatible readers may ignore the optional field. Disclosure is
fail-closed before persistence, capture and export: only validated numeric
DeviceDataCtrlr request-limit identities initially disclose values. Unknown/vendor
and WriteOnly values are redacted, while statusInfo/customData/valuesList are omitted.
This additive result contract does not widen privileged ingress, change topology
or grants, implement writes/monitoring, or certify full device-model coverage.

The EMS schema endpoints follow the listener's existing authentication policy: when
credentials are configured, unauthenticated access is denied; the documented no-credentials
loopback policy is unchanged. Current paths are
`/bridge/v1/schemas/v1.5/command-result.schema.json`,
`/bridge/v1/schemas/v1.6/export-record.schema.json` and
`/bridge/v1/schemas/v1.6/export-batch.schema.json`. Historical routes, including v1.4
command-result and v1.5 exports, remain served; canonical OpenAPI references use
each contract's current revision.
`cargo test --locked -p uob-contracts` covers serialized payload validation, optional/additive
compatibility and old-result readability. EMS integration verification uses the independent
probe executable as configured by `scripts/verify-workspace.sh`.

MQTT's existing immediate and durable result publishers accept additive v1.5 on
existing topics alongside supported v1.0/v1.1/v1.4 results. The v1.2/v1.3 policy is
unchanged: this addition does not promise every v1 minor revision or add a MQTT
command family. The EMS integration listener retains exact-origin/principal status
ownership and does not grant privileged query submission through target ingress.
Existing target payload caps remain unchanged: an oversized serialized result
produces an explicit delivery/response error, not truncation, dropped evidence or
a success-shaped fallback. MQTT broker PUBACK remains delivery acknowledgement,
never native acceptance, complete inventory or physical charging success.
