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

The EMS HTTP contract retains released command-result schemas through v1.8 and serves
the current v1.9 command-result; OpenAPI command result responses reference v1.9.
Current nested export-record and export-batch references use v1.10, while released
export schemas through v1.9 and their historical routes remain available unchanged.

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

OCPP 1.6J full privileged `SetChargingProfile`/`ClearChargingProfile` add optional
`charging_profile_16` to command-result v1.6. This action-tagged evidence contains the
immutable typed Set request or Clear selectors and a valid native CALLRESULT:
Set `Accepted`/`Rejected`/`NotSupported`, or Clear `Accepted`/`Unknown`. Native casing is
retained for action/status; typed request fields use snake_case. Profile identity/purpose,
signed native IDs, stack/kind/optional recurrence, supplied validity/anchors/duration,
ordered periods, optional native phases and exact A/W decimal-string rates remain distinct
from canonical `SetChargingLimit`; zero is preserved. No removed-profile list or hardware
capacity is inferred. Older result JSON decodes with the optional field absent.

Valid native denials retain typed status with `protocol_rejected`. Sanitized CALLERROR
rejection has no invented CALLRESULT; malformed/timeout/disconnected replies remain
`transmission_uncertain` without fabricated `charging_profile_16`. Terminal evidence survives
conflicting writers and restart, and cannot resolve uncertainty through a late reply.
Native command recovery does not replay dispatch.

Nested export-record and export-batch advance to v1.7 because they embed CommandResult.
Historical command-result v1.0–v1.5 and export v1.0–v1.6 snapshots/routes remain unchanged.
The optional/additive compatibility rule does not weaken strict privileged command ingress
or grant ordinary control permission to native profile actions. The 1.6 profile evidence
addition did not need a SQL migration. HTTP/MQTT support here is the existing scoped
result-reader/publisher contract, not a new ingress family or automatic native-command
external-export producer. An available export schema is not evidence of global export delivery.

OCPP 2.0.1 full privileged `SetChargingProfile`/`ClearChargingProfile` add the separate,
optional `charging_profile_201` to command-result v1.7. Typed Set evidence retains native
station/EVSE scope, signed profile/schedule IDs, the native transaction string, purpose,
kind, optional recurrence/validity/anchors/duration and ordered periods. Rates and minimum
rates are exact decimal strings including zero; omitted optional phases remain omitted.
Clear evidence retains either the profile ID or nested AND criteria, without inferring
removed profile IDs or complete inventory. Set uses `Accepted`/`Rejected`; Clear uses
`Accepted`/`Unknown`. Only allowlisted `NotSupported`/`InvalidValue`/`NotFound` reasons may
appear as `reason_code`; opaque additional information and extensions are not evidence.

Canonical `SetChargingLimit`, malformed responses, CALLERROR and transmission uncertainty
do not acquire this full native evidence. Native Accepted is acknowledgement, not physical
charging or enforcement proof. Nested export-record/export-batch advance to v1.8; all earlier
published snapshots remain unchanged. The native201 ownership ledger separately adds SQLite
schema v14, which older schema-v13 binaries reject; additive JSON compatibility does not imply
database rollback compatibility. Full native Set opt-in requires explicit operator initialization
with three station-privileged all-EVSE purpose-only Clears, Accepted or Unknown, for
ChargingStationMaxProfile, TxDefaultProfile and TxProfile. No automatic Clear occurs and these
operator actions can remove existing charging policies; canonical-only configurations without
full native Set enabled do not gain this prerequisite.

OCPP 2.0.1 protected `SetVariables`/`SetNetworkProfile` writes add optional typed
`configuration_201` to command-result v1.8. SetVariables evidence retains each
component/variable identity, attribute type and exact native `Accepted`, `Rejected`,
`UnknownComponent`, `UnknownVariable`, `NotSupportedAttributeType` or `RebootRequired`.
Mixed results preserve every item with aggregate `accepted: false`; aggregate acceptance
requires every item to be `Accepted` or `RebootRequired`. Neither status proves an
independently observed change, and `RebootRequired` does not trigger an automatic reboot.

SetNetworkProfile evidence retains the full signed 32-bit `configuration_slot`, including
zero, exact native `Accepted`/`Rejected`/`Failed`, and a separate `staged` fact. Native
Accepted means staged, not active connectivity, even when replacing the active slot.
Activation requires a separate operator-controlled reboot; reconnect or restart cannot
establish activation. Typed configuration evidence contains no values, profiles,
`statusInfo`, `customData` or reusable capabilities. Malformed or uncorrelated replies,
CALLERROR and transmission uncertainty do not acquire fabricated typed native outcomes.

Nested export-record/export-batch public schemas advance to v1.9 because they embed
CommandResult. **Published schema version v1.9 and runtime export envelope revision 8
are intentionally separate namespaces**: `ExportRecord::SCHEMA_VERSION` and
`ExportBatch::SCHEMA_VERSION` identify runtime revision 8. Retained historical record
versions are not rewritten. Released command-result schemas through v1.7 and export
schemas through v1.8 remain byte-identical; old result JSON still decodes with
`configuration_201` absent, and compatible older readers may ignore the optional field.
This addition requires no SQL migration; the existing SQLite schema remains v14.

The initial public protected-envelope schemas are
`v1.0/set-variables-reference-201.schema.json` and
`v1.0/set-network-profile-reference-201.schema.json`, with `$id` prefixes
`https://schemas.universal-ocpp-bridge.dev/contracts/`. Their privileged payload identities
are respectively `urn:uob:ocpp201:SetVariablesReference:1` and
`urn:uob:ocpp201:SetNetworkProfileReference:1`; these URNs are not native OCA request
identities or public schema revision numbers. SetVariables carries `setVariableData`
identities and `valueReference`, never `attributeValue`; SetNetworkProfile carries signed
`configurationSlot` and `profileReference`, never `connectionData`. The envelopes reject
unknown fields. Only protected-reference envelopes are published here, not the
secret-bearing native network profile input schema. Serving these schemas grants no
privileged target ingress, protected-value provider or automatic export production.

The EMS schema endpoints follow the listener's existing authentication policy: when
credentials are configured, unauthenticated access is denied; the documented no-credentials
loopback policy is unchanged. At the configuration-evidence introduction, paths were
`/bridge/v1/schemas/v1.8/command-result.schema.json`,
`/bridge/v1/schemas/v1.9/export-record.schema.json` and
`/bridge/v1/schemas/v1.9/export-batch.schema.json`. Historical routes, including v1.7
command-result and v1.8 exports, remain served; canonical OpenAPI references use
each contract's current revision. The initial protected-reference schemas are served at
`/bridge/v1/schemas/v1.0/set-variables-reference-201.schema.json` and
`/bridge/v1/schemas/v1.0/set-network-profile-reference-201.schema.json`.
`cargo test --locked -p uob-contracts` covers serialized payload validation, optional/additive
compatibility and old-result readability. EMS integration verification uses the independent
probe executable as configured by `scripts/verify-workspace.sh`.

The configuration-evidence addition explicitly admitted named v1.8 (revision 8)
on existing MQTT topics alongside supported v1.0/v1.1/v1.4/v1.5/v1.6/v1.7.
The later local-list addition also explicitly admits v1.9. Current publishers reject
future revision 10 before broker handoff, while v1.2/v1.3 remain unsupported. This
does not promise every v1 minor revision or add a privileged MQTT command family.
The EMS integration listener retains exact-origin/principal/resource status ownership and
does not grant privileged native profile or configuration writes through target ingress.
Neither target gains privileged native command authority or a protected-value provider;
result/schema consumption does not enable automatic external-export production.
Existing target payload caps remain unchanged: an oversized serialized result produces
an explicit delivery/response error, not truncation, dropped evidence or a success-shaped
fallback. MQTT broker PUBACK remains packet-correlated delivery acknowledgement, never
native acceptance, complete inventory, profile activation/enforcement or physical success.

The focused consumer regressions are `charging_profile201_results` and
`configuration201_results` in the EMS adapter, and `charging_profile201_wire` and
`configuration201_wire` in the MQTT adapter. They exercise current and nested schema
consumption, historical readability, exact origin/principal/resource and host-grant
boundaries, per-item native outcomes and staged-only network evidence, signed slot
bounds, 1024-period charging-profile evidence, explicit encoded-payload failures, and
immediate/durable broker acknowledgement isolation. Run them after generating the latest
canonical schemas and EMS OpenAPI snapshot; the commands below are verification
instructions, not a claim that the checks have passed:

```text
cargo test --locked -p uob-ems-scada-http-target-adapter --test charging_profile201_results
cargo test --locked -p uob-mqtt-target-adapter --test charging_profile201_wire
cargo test --locked -p uob-ems-scada-http-target-adapter --test configuration201_results
cargo test --locked -p uob-mqtt-target-adapter --test configuration201_wire
```

OCPP 1.6 `GetLocalListVersion`, protected `SendLocalList` and `ClearCache` add
optional action-tagged `local_authorization_16` to command-result v1.9. Queries
retain the exact signed 32-bit reported version, including empty `0` and unsupported
`-1`. Send retains requested version/update type and native `Accepted`, `Failed`,
`NotSupported` or `VersionMismatch`; Clear retains `Accepted` or `Rejected`.
Native casing is preserved for action/status; safe typed fields use snake_case.
Evidence contains no entries, parent identities, capabilities or arbitrary native
errors. Accepted ACK is not retained-content, offline-authorization or physical
effect proof. Malformed, unpaired, lost and CALLERROR responses do not manufacture
this field; old results still decode with it absent.

The public protected envelope is
`v1.0/send-local-list-reference-16.schema.json`, payload identity
`urn:uob:ocpp16:SendLocalListReference:1`; it contains `listVersion`, `updateType`
and an exact bound `updateReference`, not the native OCA list payload.
Query/Clear use the pinned native empty request objects. Strict privileged ingress
and per-resource capability/grant checks remain unchanged.

Nested export-record/export-batch public snapshots advance to v1.10, with
**separately named runtime envelope revision 9**. Earlier snapshots/routes remain
unchanged; this addition has no SQL migration and SQLite remains v14. Existing
HTTP/MQTT result transport does not grant new privileged target ingress, create an
authoritative service-side station list or enable automatic global export delivery.
MQTT immediate and durable consumers preserve all typed native statuses and query
versions under the explicit v1.9 result gate, existing topic ownership, payload caps
and packet-specific PUBACK rules. A PUBACK is only broker receipt, never evidence of
installed contents or offline authorization; future revision 10 is unsupported.
