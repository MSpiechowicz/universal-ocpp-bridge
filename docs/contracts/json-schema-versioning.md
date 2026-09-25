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

The EMS HTTP contract serves both the earlier v1.0 and current v1.1 command-result
schemas; command result responses reference the current version in OpenAPI.
