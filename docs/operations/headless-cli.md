# Headless service and release CLI

The production binary is `uob`. Every command is noninteractive: it never reads a prompt from
standard input and never launches a browser. Service diagnostics go to standard error; commands
that produce machine-readable records use standard output only.

## Configuration

Both startup and offline validation read the same strict TOML document. A minimal API-only demo is:

```toml
[bridge]
id = "site-01"
environment = "demo"

[management]
listen_addr = "127.0.0.1:8080"
```

`environment` defaults to `production`, and the management listener defaults to
`127.0.0.1:8080`. The current service fails closed on non-loopback management listeners because
the TLS listener is not yet composed. Unknown sections and fields are rejected.

When a target is configured, `bridge.target_id` must name the single enabled `[[targets]]` entry.
Target `settings` are converted to the shared typed configuration boundary; names ending in
`_file` or containing `credential` are credential references, not inline secret values. The
registry still rejects kinds whose concrete factory is not present in this build.

The concrete outbound MQTT target, its TLS/plaintext rules, topic taxonomy, and broker integration
test are documented in [`MQTT target`](../configuration/mqtt-target.md). The direct integration
listener is documented in
[`EMS/SCADA HTTP target`](../configuration/ems-scada-http-target.md).

## Demo charging station views

To display real station observations, explicitly extend an isolated **demo** configuration.
Both listeners must bind loopback; plaintext charging ingress is rejected outside `demo`.
The read grant is independent of charger credentials and authorizes only the configured
station roster, not diagnostics capture, commands or an outbound target:

```toml
[bridge]
id = "local-demo"
environment = "demo"

[management]
listen_addr = "127.0.0.1:8080"

[charging]
enabled = true
listen_addr = "127.0.0.1:9000"
state_directory = "/var/lib/uob/demo-private"
read_grant_file = "/var/lib/uob/demo-secrets/management-read"

[[charging.stations]]
id = "station-a"
protocol = "ocpp16j"
credential_file = "/var/lib/uob/demo-secrets/station-a"

[[charging.stations.resources]]
connector_id = "connector-1"
native_connector_id = 1

[[charging.stations]]
id = "station-b"
protocol = "ocpp201"
credential_file = "/var/lib/uob/demo-secrets/station-b"

[[charging.stations.resources]]
evse_id = "evse-1"
native_evse_id = 1

[[charging.stations.resources]]
evse_id = "evse-1"
connector_id = "connector-1"
native_evse_id = 1
native_connector_id = 1

[[charging.stations.resources]]
evse_id = "evse-2"
native_evse_id = 2

[[charging.stations.resources]]
evse_id = "evse-2"
connector_id = "connector-2"
native_evse_id = 2
native_connector_id = 1
```

Create the state directory owned by the service account with mode `0700`; create
distinct credential files owned by the same account with mode `0600`, no symlinks
or hard links. Generate independent random station secrets of at least 16 bytes.
The read file must contain one complete `uob1.demo.`-prefixed bearer token with
32–128 printable secret characters and no newline. Protect the directory containing
those files; never put real credentials in TOML, version control, process arguments,
browser URLs or logs. Supply each station's credential through its OCPP WebSocket
Basic-auth handshake and select `ocpp1.6` or `ocpp2.0.1` accordingly.

`uob serve` verifies privacy, credentials, topology, bridge identity and exclusive
SQLite ownership before admitting stations. A matching identity marker left by an
interrupted first start can be resumed; an unmarked existing database or a foreign
bridge marker cannot be adopted. Restart retains recent transactions, points and event
cursors; transport disconnect emits a durable change and marks availability unknown
until a fresh accepted status observation. OCPP 2.0.1 retains at most 128 transaction
rows, preserving active rows and a monotonic seven-day replay floor for aged ended rows.
If the protected recent/active history fills that bound, further starts fail closed rather
than evicting a transaction or accepting an old replay. Storage retention runs at startup
and every minute while charging is active.
The management console on `http://127.0.0.1:8080` requires manual entry of the read
token and shows only committed observations. A report of a transaction is not proof
of authorization or physical charging: with no provisioned local authorization grant
the demo replies `Invalid`. Disabling `[charging]` leaves station/event reads
unavailable (503) rather than serving a synthetic inventory. The configuration above intentionally
enables **read-only** views; do not forward plaintext charging ingress off the loopback host.

To opt in to demo commands, provision **distinct** protected grant files and add
`control_grant_file` and, for privileged actions, `privileged_grant_file` under `[charging]`.
Neither is the read grant or a station's WebSocket credential. A privileged grant requires
a control grant to be configured, but does not itself authorize standard control operations.
The files must be separate from one another, station credentials, start identities and the
private state directory; use the same owner, modes and demo-bound bearer format as the read
grant. The service rejects command opt-ins without a control grant and privileged action
opt-ins (including `change_availability`, native profiles and schedule queries) without a
privileged grant.

Enable actions individually under each `[[charging.stations]]`: `start_token_file` points to
a private local charging authorization identity for start, `allow_stop = true` enables stop,
`allow_charging_limit = true` enables charging limits, and `change_availability = true` enables
the privileged OCPP `ChangeAvailability` operation. The start token is not a management bearer:
it is resolved to a protected, station-scoped authorization reference at startup; the console
uses that reference, not the token's secret value. Missing opt-ins remain disabled. The service
advertises operations only for supported resources: start, stop and availability at station
level, charging limits on native OCPP 1.6J connectors or OCPP 2.0.1 EVSE resources with a
positive native ID. Control options and privileged schemas are authenticated per request.
Privileged actions require the privileged grant, the advertised protocol action, a pinned
server schema and valid typed fields; a displayed schema alone never grants permission.

OCPP 1.6J and 2.0.1 `TriggerMessage` each require a privileged **demo-only**
per-station opt-in. For a station in the example above, provision the distinct
control and privileged grant files first, then add these entries in their existing sections:

```toml
# Inside [charging]:
control_grant_file = "/var/lib/uob/demo-secrets/management-control"
privileged_grant_file = "/var/lib/uob/demo-secrets/management-privileged"

# Inside the existing OCPP 1.6J or OCPP 2.0.1 [[charging.stations]] for station-a:
trigger_message = true
```

These are section additions, not a standalone TOML document. The read grant
remains separate for station-scoped command detail/history; the control grant
does not authorize `TriggerMessage` by itself. A per-request privileged
credential, advertised edition-specific action, pinned request schema and
matching resource scope are required. OCPP 1.6J uses
`urn:OCPP:1.6:2019:12:TriggerMessageRequest` and permits six classes:
`BootNotification` (only before Accepted registration),
`DiagnosticsStatusNotification`, `FirmwareStatusNotification`, `Heartbeat`,
`MeterValues` and `StatusNotification`. Explicit connector 0 requests station
status only; positive connector IDs address that connector for status/metering;
an omitted ID requests all applicable configured targets. A station-only class
ignores an irrelevant connector ID. The dispatched expectation is bounded to
64 connectors and fixed at send time. Native `Accepted` is not an observed
action: within 60 seconds of dispatch start, compatible committed station
messages update `pending`/`partial`/`observed`/`absent` or native denial sets
`unsupported`. Matching messages cannot prove causation or physical charging.
An unanswered or interrupted command is not automatically retried on restart
or reconnect; view its durable status before issuing an explicit new request.

OCPP 2.0.1 instead uses `urn:OCPP:Cp:2:2020:3:TriggerMessageRequest`
and eleven native `requestedMessage` values: `BootNotification`,
`LogStatusNotification`, `FirmwareStatusNotification`, `Heartbeat`,
`MeterValues`, `SignChargingStationCertificate`, `SignV2GCertificate`,
`StatusNotification`, `TransactionEvent`, `SignCombinedCertificate` and
`PublishFirmwareStatusNotification`. The optional `evse` object has a positive
`id` and optional positive `connectorId`; it is not the 1.6 connector ID.
Station-only classes ignore an irrelevant `evse`; Boot requires registration
not yet Accepted. StatusNotification requires both EVSE and connector and
their exact connector resource. MeterValues is EVSE-wide, so a connector-only
grant cannot authorize it even if a connector ID is supplied. Scoped V2G or
combined signing requires an EVSE resource, but its station-level certificate
receipt cannot be attributed to that EVSE. Omitted EVSE expands to all
configured EVSEs (maximum 64) for applicable classes; an empty/excessive set
fails closed. Native `Accepted`, `Rejected`, `NotImplemented` and optional
`statusInfo` are distinct from compatible committed calls in the fixed
60-second observation window. Durable outcomes include `unattributable` for
EVSE-scoped certificate receipts. A reply or compatible report proves neither
causation, certificate completion nor physical charging. The simulator has no
certificate private key/CSR and returns `NotImplemented` for signing triggers.
No interrupted or uncertain dispatch is replayed after disconnect/restart.

OCPP 1.6J `GetCompositeSchedule` is independently **default off**. For station-a in the
demo example, add the following only to the existing sections after provisioning protected
credential files (the paths are placeholders, not secrets):

```toml
# Inside the existing [charging]:
control_grant_file = "/var/lib/uob/demo-secrets/management-control"
privileged_grant_file = "/var/lib/uob/demo-secrets/management-privileged"

# Inside the existing OCPP 1.6J [[charging.stations]] for station-a:
get_composite_schedule = true
```

These are additions, **not a standalone TOML document**. Keep read, control, privileged and
station credentials separate; reuse already configured grant entries rather than duplicating
TOML keys. The option requires both control and privileged grant files, but only a per-request
privileged credential authorizes the query. An opt-in station with any configured native
connector or EVSE ID above `i32::MAX` (2147483647) is rejected. The same key on an OCPP 2.0.1
station enables the separate 2.0.1 query described below. Default-off
legacy topology behavior and Demo/loopback restrictions remain unchanged.

After Accepted registration, the live station advertises `GetCompositeSchedule` for station
scope and exact configured positive connectors. Submit the existing privileged OCPP command
with protocol `ocpp16j`, action `GetCompositeSchedule` and payload schema
`urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest`. Its payload has required integer
`connectorId` and `duration`, plus optional `chargingRateUnit` (`A` or `W`).
Station scope requires connector 0 for grid aggregation; a connector-scoped command must
name its exact positive native ID. Duration is 1–2147483647 seconds. Omit an optional unit
rather than sending null. Missing required fields, nulls, unknown fields, fractional/overflow
integers, wrong schema/edition/unit, missing capability, unaccepted registration, disconnected
session and out-of-scope authority fail before a CALL.

Read the durable command detail with the separate station-scoped read grant. Optional typed
`composite_schedule_16` records the request context, exact native Accepted/Rejected status
and supplied schedule/identity/phase metadata. Accepted requires meaningful nonempty periods;
exact nonnegative native rates have at most one meaningful fractional digit and are returned
as canonical decimal strings, not rounded floats. Zero remains zero even with a positive
minimum rate. Valid Rejected is `protocol_rejected` with typed Rejected evidence; malformed or
semantically invalid CALLRESULT is `transmission_uncertain` without fabricated schedule
evidence. Valid CALLERROR remains a sanitized rejection without an invented CALLRESULT.
The schedule is the charger's indicative calculation, not a changed snapshot, local calculation,
installed profile or evidence of charging enforcement/physical success.

Duplicates return the original durable result without another send. Restart and reconnect
never replay the query, and an old connection's response cannot resolve a new request.
Inspect the original status before issuing a new explicit request after reconnect.
This opt-in does not establish simulator smart charging or certification.

OCPP 2.0.1 stations accept the same `get_composite_schedule = true` key and a separate
`get_charging_profiles = true` key (2.0.1 only); both require the control and privileged
grant files. Use protocol `ocpp201` with schema
`urn:OCPP:Cp:2:2020:3:GetCompositeScheduleRequest` and payload `evseId` (0 at station
scope for the grid connection, the exact native EVSE ID at EVSE scope), `duration` and an
optional `chargingRateUnit`. The typed `composite_schedule_201` evidence keeps the request,
the exact native status, a standardized reason code and the schedule with exact limits.

`GetChargingProfiles` uses schema `urn:OCPP:Cp:2:2020:3:GetChargingProfilesRequest` with a
signed `requestId`, an optional `evseId` (omitted for every EVSE or zero for the grid
connection at station scope; required and exact at EVSE scope) and a `chargingProfile`
criterion with either `chargingProfileId` (at most 64) or at least one of `stackLevel`,
`chargingLimitSource` and `chargingProfilePurpose`. The HTTP 202 result carries the native
acknowledgement; while `charging_profiles_201.report.state` is `pending`, poll the command
detail until it is `complete`, `incomplete` (with a reason such as `correlation`,
`timeout`, `disconnected` or `interrupted`) or `not_expected` after `NoProfiles`. A
`requestId` stays retired on that connection. Reported profiles describe the charger's
state at report time; they are not installed or enforced by the bridge.

### OCPP 2.0.1 charging needs and external limits

OCPP 2.0.1 stations always get answers to `NotifyEVChargingNeeds`,
`NotifyEVChargingSchedule`, `NotifyChargingLimit` and `ClearedChargingLimit`, and the answer
needs no command. By default, charging needs are answered `Rejected` with `NotEnabled`.

Setting `ev_charging_needs_processing = true` on a 2.0.1 station answers `Processing`
instead. The station must also set `allow_charging_limit` or `set_charging_profile`, and
commands must be enabled. Only set it when your EMS reacts to the needs by sending a
`TxProfile` through this bridge, either a canonical `set_charging_limit` on the EVSE or a
native `SetChargingProfile`. The bridge never sends one itself.

Read the outcome from the station snapshot:

- The EVSE resource carries the `ocpp201/evse-{n}/ev-charging-needs/*` and
  `ocpp201/evse-{n}/ev-charging-schedule/*` points.
- Limits appear under `ocpp201/charging-limit/{source}/*` on the station, or under
  `ocpp201/evse-{n}/charging-limit/{source}/*` on the EVSE.
- The exact needs and schedules are in the `station.*.201` journal events.

A rejected EV schedule means it exceeds the bridge's own installed limit
(`basis = exceeds_csms_schedule`), or that the bridge could not check it exactly
(`unverifiable`). Renegotiating is your decision: send a new limit or profile. See
[charging needs and external limits](../architecture/ocpp201-charging-negotiation.md).

### Full native OCPP 1.6J Set/Clear charging profiles

`set_charging_profile` and `clear_charging_profile` are independent, **default-off** station
options, separate from `allow_charging_limit`/canonical `SetChargingLimit`. Neither enables the
other. Use only isolated demo ingress with both listeners on loopback. The following complete
configuration shows table placement; replace paths with protected files provisioned as above,
not inline credentials. Station flags belong before its `[[charging.stations.resources]]` table:

```toml
[bridge]
id = "local-demo"
environment = "demo"

[management]
listen_addr = "127.0.0.1:8080"

[charging]
enabled = true
listen_addr = "127.0.0.1:9000"
state_directory = "/var/lib/uob/demo-private"
read_grant_file = "/var/lib/uob/demo-secrets/management-read"
control_grant_file = "/var/lib/uob/demo-secrets/management-control"
privileged_grant_file = "/var/lib/uob/demo-secrets/management-privileged"

[[charging.stations]]
id = "station-a"
protocol = "ocpp16j"
credential_file = "/var/lib/uob/demo-secrets/station-a"
set_charging_profile = true
clear_charging_profile = true

[[charging.stations.resources]]
connector_id = "connector-1"
native_connector_id = 1
```

For an existing configuration, add only missing keys in those same tables. Both grant files
are required for either opt-in, but **only the per-request privileged bearer** authorizes
these OCPP actions. The ordinary control bearer, read bearer and station Basic-auth credential
cannot substitute for it. Native IDs above 2147483647 fail configuration validation;
OCPP 2.0.1 uses the separate EVSE semantics below, not these connector payloads.
After Accepted registration, query advertised exact-resource options before submitting.
Live resource, socket generation, capability, privilege and expiry are rechecked before dispatch.

Here is a complete programmatic management submission, not a browser editor. Angle-bracket
bearers below are placeholders; load real credentials through the client's protected credential
facility, never process arguments, logs or URLs. Replace `expires_at` with a short future
RFC 3339 deadline and use a fresh request ID for each intentional new command:

```http
POST /api/v1/commands HTTP/1.1
Host: 127.0.0.1:8080
Authorization: Bearer <management-privileged>
Content-Type: application/json

{
  "request_id": "profile-set-001",
  "resource": {
    "bridge_id": "local-demo",
    "station_id": "station-a",
    "resource": {"kind": "connector", "connector_id": "connector-1"},
    "native_protocol_reference": {"protocol": "ocpp16", "connector_id": 1}
  },
  "operation": {
    "kind": "ocpp",
    "parameters": {
      "protocol": "ocpp16j",
      "action": "SetChargingProfile",
      "payload_schema": "urn:OCPP:1.6:2019:12:SetChargingProfileRequest",
      "payload": {
        "connectorId": 1,
        "csChargingProfiles": {
          "chargingProfileId": -117,
          "stackLevel": 2,
          "chargingProfilePurpose": "TxDefaultProfile",
          "chargingProfileKind": "Relative",
          "chargingSchedule": {
            "chargingRateUnit": "A",
            "chargingSchedulePeriod": [
              {"startPeriod": 0, "limit": 8.1, "numberPhases": 4},
              {"startPeriod": 60, "limit": 0}
            ]
          }
        }
      }
    }
  },
  "expires_at": "2099-01-01T00:00:00Z"
}
```

Native numeric rates use exact nonnegative tenths in `A` or `W`, not canonical unit strings.
Zero and positive native phases above three are legal; this example's four phases is requested
metadata, not a claim about installed hardware. Extra meaningful fractional digits, negative
rates, precision loss and overflow fail without rounding. Profiles retain signed i32 IDs,
nonnegative stack and optional supplied validity/anchors/duration/minimum rate. `recurrencyKind`
is optional and only allowed for `Recurring`; omitted fields stay absent, not null. Periods
must start at 0, strictly increase, and number at most 1024. Native zero duration and periods
beyond duration/recurrence are retained for charger truncation. Profile CALLs have a 256 KiB
fully encoded ceiling in addition to existing tighter API/runtime budgets.

`ChargePointMaxProfile` uses a station resource (only bridge/station IDs) and `connectorId: 0`.
`TxDefaultProfile` permits station 0 or an exact connector as above. `TxProfile` instead requires
the exact positive connector and a `transactionId` read from that resource's unique ongoing
native OCPP 1.6 transaction. Pending/Active/Suspended is eligible only after the native identity
is established; missing, ended, uncertain or ambiguous transactions fail before wire. Other
purposes prohibit `transactionId`. Native options do not change canonical charging-limit
conversion, positive-limit or 1–3-phase behavior.

To clear the profile by ID, submit this separate full command using **station authority**:

```http
POST /api/v1/commands HTTP/1.1
Host: 127.0.0.1:8080
Authorization: Bearer <management-privileged>
Content-Type: application/json

{
  "request_id": "profile-clear-001",
  "resource": {"bridge_id": "local-demo", "station_id": "station-a"},
  "operation": {
    "kind": "ocpp",
    "parameters": {
      "protocol": "ocpp16j",
      "action": "ClearChargingProfile",
      "payload_schema": "urn:OCPP:1.6:2019:12:ClearChargingProfileRequest",
      "payload": {"id": -117}
    }
  },
  "expires_at": "2099-01-01T00:00:00Z"
}
```

Clear `id` overrides every other selector, so adding `connectorId: 1` cannot make an ID clear
child-scoped. Empty/broad/wildcard or connector-0 clears also require station authority.
A child-scoped clear must omit `id`, address the exact connector resource and supply its native
`connectorId`; optional `chargingProfilePurpose` and `stackLevel` filters combine with AND.
The response does not enumerate removed IDs.

HTTP 202 means durable admission, not native acceptance. Read `GET
/api/v1/commands/profile-set-001` or the clear request's status URL using
`Authorization: Bearer <management-read>` (the independent station-scoped read grant).
Optional action-tagged `charging_profile_16` keeps typed snake_case request/selector evidence
and Set `Accepted`/`Rejected`/`NotSupported` or Clear `Accepted`/`Unknown`. Valid native denials
are `protocol_rejected`; a valid CALLERROR is sanitized rejection without a fabricated reply.
Malformed replies, timeout or disconnect stay `transmission_uncertain`. Terminal evidence
survives restart and cannot be rewritten by a late reply. Exact duplicates return it without
another CALL; changed content under the same request ID conflicts. Restart/reconnect never
replays uncertain work: inspect the original status before a new explicit request.

The console's bounded nested schema metadata compatibility lets existing controls load when
native profiles are enabled, but the complex Set browser editor remains unsupported. These
native commands do not compute/enforce a local schedule, certify hardware, provide a full
simulator or automatically produce global external exports. Native16 has no profile-specific
migration; current SQLite schema14 adds the separate native201 ownership ledger. See
[native16 semantics](../architecture/ocpp16-remote-control.md) and
[current result/schema versions](../contracts/json-schema-versioning.md).

### Full native OCPP 2.0.1 Set/Clear charging profiles

For an existing `[[charging.stations]]` with `protocol = "ocpp201"`, use the same independently
default-off `set_charging_profile` and `clear_charging_profile` boolean keys. Keep both
listeners loopback and the demo-only, independent privileged grant above. Native profile
actions are advertised for the station and configured positive EVSE-only resources, not
connector resources. Native IDs must fit signed32-bit integers; configured EVSE IDs are positive.
GetVariables is independently default-off, with no automatic phase-capability query.

**Destructive operational prerequisite:** enabling full native Set blocks both native Set and
canonical SetChargingLimit until three explicit station-privileged, all-EVSE purpose-only
Clear requests receive valid Accepted or Unknown replies. These remove existing station
policies; do this only when exclusive CSMS ownership is established. The bridge does not
discover or fabricate the charger's full inventory and never sends these clears automatically.
Repeat the following command envelope with distinct request IDs and purpose values
ChargingStationMaxProfile, TxDefaultProfile and TxProfile; inspect each retained result.
Partial baseline progress survives restart. External constraints are protected.

```json
{
  "request_id": "profile201-baseline-default-001",
  "resource": { "bridge_id": "bridge-1", "station_id": "station-b" },
  "operation": {
    "kind": "ocpp",
    "parameters": {
      "protocol": "ocpp201",
      "action": "ClearChargingProfile",
      "payload_schema": "urn:OCPP:Cp:2:2020:3:ClearChargingProfileRequest",
      "payload": { "chargingProfileCriteria": { "chargingProfilePurpose": "TxDefaultProfile" } }
    }
  },
  "expires_at": "2099-01-01T00:00:00Z"
}
```

Submit only through the existing per-request privileged `POST /api/v1/commands` path.
Clear uses `chargingProfileId` alone **or** nonempty nested chargingProfileCriteria containing
EVSE, purpose and/or stack filters combined with AND. Mixed/empty selectors and native16
spellings are invalid. ID/broad clears require station permission; a positive EVSE criterion
may use its exact EVSE resource. Omitted EVSE means all EVSEs; zero means station-owned
profiles, not all EVSEs. A targeted Unknown does not release uncertain ownership metadata.

Native Set's schema is `urn:OCPP:Cp:2:2020:3:SetChargingProfileRequest`. Its wire payload has
evseId and chargingProfile with signed id, nonnegative stackLevel, purpose/kind, optional
transactionId/recurrencyKind/validFrom/validTo, and exactly one chargingSchedule array entry.
Each schedule has signed id, A/W chargingRateUnit and 1–1024 strictly increasing first-zero
periods with exact nonnegative tenths limits, optional numberPhases and phaseToUse. Optional
duration/startSchedule/minChargingRate omissions remain absent. Absolute/Recurring requires
startSchedule; Relative prohibits it; only Recurring requires Daily/Weekly recurrencyKind.
ChargingStationMaxProfile addresses station0 and is not Relative. TxProfile needs one
established ongoing native transaction on that exact EVSE in the current generation.
Unknown fields, customData, ISO multiple schedules and salesTariff fail before CALL.

phaseToUse requires numberPhases=1 and phase1..3, plus strictly true current-generation,
correlated GetVariables Actual proof for exact SmartChargingCtrlr.ACPhaseSwitchingSupported
at that positive EVSE, no instances or connector. false/unknown/failure/malformed/out-of-order
responses revoke proof. Reconnect/restart never restores snapshot authority. No native Set
with phase selection is allowed at station0. Omitting phaseToUse does not require this proof.

One mutation per station is active. Different-ID purpose/stack/EVSE conflicts, station0 versus
positive-EVSE TxDefault conflicts and same-stack/transaction TxProfile conflicts fail before
wire. IDs are station-global: an EVSE-only replacement cannot erase another known/uncertain
scope's profile. Explicit station ID Clear followed by exact-EVSE Set is the recovery path.
At most128 metadata footprints are retained, including old+candidate uncertain replacement.
History pruning never frees live ownership; actual transaction Ended does. Clear remains
available at capacity. Canonical-only mode with full Set off needs no baseline and still
records known metadata without adding full native result evidence.
Canonical same-ID replacement additionally requires exact known metadata compatibility
(current transaction, purpose, stack, scope and validity); it cannot use ordinary control
permission to replace an incompatible privileged policy. Quantity-only repeats remain allowed.

Optional action-tagged `charging_profile_201` in command-result v1.7 preserves the complete
validated typed request and Set Accepted/Rejected or Clear Accepted/Unknown reply, plus only
allowlisted reason codes. No status is fabricated for CALLERROR/malformed/timeout/uncertainty;
opaque statusInfo additionalInfo/customData is not disclosed. Terminal evidence and ledger
changes commit atomically; duplicates never resend. Read retained results before issuing any
new reconciliation request. Embedded export v1.8 supports this evidence without adding a
global native-command producer, privileged target ingress or real PostgreSQL delivery claim.
SQLite13 migrates additively to14; a13 binary rejects14 and automatic downgrade is not qualified.

The browser can load both editions' full bounded descriptors while existing controls remain
usable. Complex native Set/Clear composition remains unsupported; there is no raw-JSON
editor. Independent software-peer installed state and native acknowledgement are not proof
of hardware enforcement or OCA certification. See
[native201 safety and proposed verification commands](../architecture/ocpp201-remote-control.md#opt-in-native-charging-profiles).

### Protected OCPP 2.0.1 device and network configuration

`set_variables` and `set_network_profile` independently default to `false`. Either
requires `protocol = "ocpp201"`, the existing distinct control/privileged grant
files and a non-secret absolute `configuration_values_file` path under `[charging]`.
Only the **per-request privileged bearer** authorizes these commands. Keep both
listeners loopback and the environment `demo`; station Basic authentication and
the read/control grants do not grant configuration-write authority.

This complete example shows table placement, not real credentials:

```toml
[bridge]
id = "bridge-1"
environment = "demo"

[management]
listen_addr = "127.0.0.1:8080"

[charging]
enabled = true
listen_addr = "127.0.0.1:9000"
state_directory = "/var/lib/uob/demo-private"
read_grant_file = "/var/lib/uob/demo-secrets/management-read"
control_grant_file = "/var/lib/uob/demo-secrets/management-control"
privileged_grant_file = "/var/lib/uob/demo-secrets/management-privileged"
configuration_values_file = "/var/lib/uob/demo-secrets/configuration201.json"

[[charging.stations]]
id = "station-a"
protocol = "ocpp201"
credential_file = "/var/lib/uob/demo-secrets/station-a"
set_variables = true
set_network_profile = true
# Independent query opt-in; writes do not enable this or issue probes:
get_variables = true

[[charging.stations.resources]]
evse_id = "evse-one"
native_evse_id = 1

[[charging.stations.resources]]
evse_id = "evse-one"
connector_id = "one"
native_evse_id = 1
native_connector_id = 1
```

For an existing configuration, add only missing keys in their existing tables;
flags precede the station's resource tables. `get_variables`, `get_base_report`
and `get_report` retain separate default-off permissions. A provisioned entry for
a disabled write, non201 station or any resource that is not an exact configured
canonical/native address fails startup. Network profiles are station-only;
variable entries may use the station or an exact configured EVSE/connector.

#### Private startup file

Both root arrays are required, including an empty array for an unused entry kind.
Root/entry objects reject unknown fields. Outer names use snake_case; `entry` is
the protected native-style component/variable reference DTO and uses camelCase.
`profile` is a complete native `connectionData` **object**, not a JSON string.
The following values and 256-bit-shaped capabilities are **synthetic examples**:
do not copy these predictable examples into real provisioning. Generate each
capability independently with a cryptographically secure 32-byte random source;
use `cfg201:` plus its 64 hexadecimal digits, not a content hash or counter.

```json
{
  "variables": [
    {
      "resource": { "bridge_id": "bridge-1", "station_id": "station-a" },
      "entry": {
        "component": { "name": "VendorCtrlr" },
        "variable": { "name": "DemoValue" },
        "valueReference": "cfg201:2f4c19b8a6d30e759bc17a05e2d846f19e07c5d2b8a3416fa09de753c1264b80"
      },
      "value": "",
      "expires_at": "2099-01-01T00:00:00Z"
    }
  ],
  "network_profiles": [
    {
      "resource": { "bridge_id": "bridge-1", "station_id": "station-a" },
      "configuration_slot": 0,
      "reference": "cfg201:7ac9e0124bd63f85d190a6b3e8c247f0536d12ea94b80fc72e5a6d8139cf042b",
      "profile": {
        "ocppVersion": "OCPP20",
        "ocppTransport": "JSON",
        "ocppCsmsUrl": "wss://csms.invalid/ocpp",
        "messageTimeout": 30,
        "securityProfile": 2,
        "ocppInterface": "Wired0",
        "apn": {
          "apn": "demo.invalid",
          "apnUserName": "demo",
          "apnPassword": "demo-only-password",
          "simPin": 0,
          "preferredNetwork": "20404",
          "useOnlyPreferredNetwork": false,
          "apnAuthentication": "PAP"
        },
        "vpn": {
          "server": "vpn.invalid",
          "user": "demo",
          "group": "demo",
          "password": "demo-only-password",
          "key": "demo-only-key",
          "type": "IKEv2"
        }
      },
      "expires_at": "2099-01-01T00:00:00Z"
    }
  ]
}
```

Choose real expiry deadlines appropriate to the operation; the long example
deadline is not a recommended policy. Bind each capability immutably to this
exact resource, content and expiry plus full variable identity/attribute or signed
slot. Omitted `attributeType` means `Actual`; comparisons use Unicode case folding
for component/variable names and instances while preserving original metadata.
Capabilities themselves compare exactly. For a child entry, copy its exact
configured ResourceRef (canonical `kind = "evse"` and matching native address)
and put its contained native EVSE/connector selector in `entry.component.evse`.
Do not infer an EVSE-only resource merely because a connector is configured.

Create the file as a canonical absolute, service-owned **0600 regular file**,
with one link, no symlink components and a protected containing directory. Keep
it outside the `0700` state directory and distinct from every grant, station
credential and start-identity file; opened inode identity and NOFOLLOW checks
also reject aliases/races. Startup reads at most 2 MiB (2,097,152 bytes), separately
from the provider's 1 MiB (1,048,576 bytes) aggregate secret-content bound.
There are at most 128 combined entries, each profile at most 64 KiB (65,536 bytes)
and JSON depth 16, and each variable at most 1,000 Unicode characters, including
legal empty strings. Component/variable names and instances are at most 50 Unicode
characters; resource identity strings are at most 256 bytes each. Native profile
fields obey the pinned schema's own bounds, including CSMS/APN/VPN string lengths
and signed 32-bit integer fields. Slot 0 is valid; native station policy can still
reject a slot or setting. Do not put private values/profiles in TOML, environment
variables, command arguments, public API requests, logs or version control.
Treat reusable capabilities as sensitive too.

#### Reference-only command API

After Accepted registration, submit through the existing privileged
`POST /api/v1/commands` endpoint. The two protected URNs below are **not** the
native OCA SetVariablesRequest/SetNetworkProfileRequest URNs. Raw `attributeValue`
or `connectionData`, unknown fields, wrong schemas and uncontained selectors fail
closed. These public envelopes match the synthetic private file above:

```json
{
  "request_id": "demo-variable-001",
  "resource": { "bridge_id": "bridge-1", "station_id": "station-a" },
  "operation": {
    "kind": "ocpp",
    "parameters": {
      "protocol": "ocpp201",
      "action": "SetVariables",
      "payload_schema": "urn:uob:ocpp201:SetVariablesReference:1",
      "payload": {
        "setVariableData": [
          {
            "component": { "name": "VendorCtrlr" },
            "variable": { "name": "DemoValue" },
            "valueReference": "cfg201:2f4c19b8a6d30e759bc17a05e2d846f19e07c5d2b8a3416fa09de753c1264b80"
          }
        ]
      }
    }
  },
  "expires_at": "2099-01-01T00:00:00Z"
}
```

```json
{
  "request_id": "demo-network-001",
  "resource": { "bridge_id": "bridge-1", "station_id": "station-a" },
  "operation": {
    "kind": "ocpp",
    "parameters": {
      "protocol": "ocpp201",
      "action": "SetNetworkProfile",
      "payload_schema": "urn:uob:ocpp201:SetNetworkProfileReference:1",
      "payload": {
        "configurationSlot": 0,
        "profileReference": "cfg201:7ac9e0124bd63f85d190a6b3e8c247f0536d12ea94b80fc72e5a6d8139cf042b"
      }
    }
  },
  "expires_at": "2099-01-01T00:00:00Z"
}
```

Use the independent read bearer for `GET /api/v1/commands/<request_id>` and command
history. A fresh missing/expired/revoked or wrongly bound reference is rejected
before admission (normally 422 for an unoffered protected command); its `GET` is 404.
Malformed schemas/payloads fail 400 before admission. A valid admitted command
definitely not sent because of native item/byte budgets instead returns HTTP 400
with a **flat** `CommandResult` whose lifecycle is `rejected`; its retained `GET`
is 200 and matches that result. Do not assume every 400 has only `{error}`, or
that every rejected command was sent. Valid native responses and transmission
uncertainty use the existing 202 `{result}` envelope, not proof of physical effect.

SetVariables preserves Accepted, Rejected, UnknownComponent, UnknownVariable,
NotSupportedAttributeType and RebootRequired independently. Reordered complete
replies are valid; missing/duplicate/extra/mismatched identities are uncertain.
Aggregate acceptance is true only when every item is Accepted or RebootRequired;
mixed outcomes remain visible with aggregate false. ReadOnly, malformed-format
and out-of-range decisions remain the station's genuine Rejected evidence.

Unknown SetVariables limits permit only a single item. Before multi-item writes,
explicitly query DeviceDataCtrlr (no component instance/EVSE), variable
ItemsPerMessage and BytesPerMessage, each with variable instance `SetVariables`
and Actual attribute. GetVariables has its own independent opt-in; query these
identities one at a time when its own limits are unknown. Valid NotifyReport
evidence can also teach limits through independently enabled report workflows.
Both native item and byte limits are needed for multi-item writes. Local bounds
are 4,096 entries and a 256 KiB **complete escaped native CALL**, including resolved
values and framing, not merely the reference JSON size. Reconnect/restart loses
the shared generation's learned proof; stored query results do not restore it.
There is no automatic querying, splitting, truncation, retry or replay.

SetNetworkProfile carries every supplied validated native field, including
APN/VPN credentials and version/transport/interface. The bridge does not resolve
or fetch its URL. Native Accepted means **stored/staged**, even when replacing
the currently active slot; it is not active before a separate operator-controlled
reboot. Rejected and Failed remain exact native statuses. No automatic Reset,
B10 migration, security-profile orchestration or connectivity probe is issued.
Disconnect after possible application but before ACK is transmission uncertainty;
reconnect alone proves neither activation nor a safe resend.

Optional `configuration_201` in command-result v1.8 contains identities or slot,
native status and network staging only, never values/profiles/capabilities or
opaque statusInfo/customData. Nested export schemas are v1.9 and runtime export
revision is 8; historical schemas/routes remain intact and existing payload and
target authorization limits remain unchanged. SQLite stays at schema 14 with no
new SQL migration or configuration ledger. This does not enable global external
export delivery, a provider or a native configuration browser editor.

#### Replacement, revocation and restart

There is **no hot reload, watcher or public secret/revocation endpoint**.
To change content or expiry, stop the daemon, produce a new service-owned mode `0600`
private file with independently generated fresh capabilities, atomically replace
the original file without links/overlap, then restart. Omit a capability to revoke
it from that startup set. The embedding library also offers explicit
`LocalConfigurationValues201::revoke(reference)`, not a daemon HTTP operation.
Revocation and expiry are rechecked before actual send; after sending begins,
the bridge cannot unsend bytes or prove that an interrupted update was unapplied.
Owned private/decoded/transient buffers wipe on drop/error; third-party buffers,
OS copies and transmitted bytes are outside that guarantee.

Startup pages common unresolved-command recovery before listener binding.
Crash-left Dispatched becomes TransmissionUncertain; Admitted and already
uncertain records are preserved, with cursor progress past them and no replay.
Inspect durable results before a new intentional request. An exact retained
request under the same authorized origin/resource may return its historical
result even after capability expiry/replacement, without resolving the old secret
or another CALL. A changed body under that ID, wrong resource or ordinary control
grant does not gain cached privileged authority.

The actual-daemon configuration suite passed 13 tests, and an independent
software-peer smoke exercised 15 native writes, 20 durable commands/results and 190
capture traces, including status/limits/scope/expiry/rotation, pending Heartbeat,
staged peer reboot and crash/disconnect no replay. Synthetic private values/
profiles/capabilities were absent from public results and enabled redacted capture;
private values were absent from SQLite/WAL and logs. This is software boundary
evidence, not hardware activation/interoperability, OCA certification or production
charging qualification. See [the architecture and safety boundaries](../architecture/ocpp201-remote-control.md#opt-in-protected-device-and-network-writes).

### Protected OCPP 1.6 station authorization list and cache

`get_local_list_version`, `send_local_list` and `clear_cache` are independent,
default-off, privileged **demo-only** station options. They require `protocol =
"ocpp16j"`, separate control/privileged grants and the existing protected charging
state directory. They neither enable each other nor change the service's independent
exact-byte SHA allowlist. They add no production control or public provisioning API.

```toml
[charging]
enabled = true
listen_addr = "127.0.0.1:9000"
state_directory = "/srv/uob-demo/private/charging"
read_grant_file = "/srv/uob-demo/private/read.grant"
control_grant_file = "/srv/uob-demo/private/control.grant"
privileged_grant_file = "/srv/uob-demo/private/privileged.grant"
local_authorization_updates_file = "/srv/uob-demo/private/local-list.json"

[[charging.stations]]
id = "demo-1"
protocol = "ocpp16j"
credential_file = "/srv/uob-demo/private/demo-1.credential"
get_local_list_version = true
send_local_list = true
clear_cache = true
```

The startup-only provider file is service-owned mode `0600`, under a protected
owner-only canonical directory, outside the charging state directory and without
credential aliases. Its exact shape is:

```json
{
  "updates": [{
    "station_id": "demo-1",
    "update_reference": "list16:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "expires_at": "2099-01-01T00:00:00Z",
    "request": {
      "listVersion": 17,
      "updateType": "Full",
      "localAuthorizationList": [{
        "idTag": "demo-native-tag",
        "idTagInfo": {"status": "Accepted"}
      }]
    }
  }]
}
```

The reference above is illustrative; generate an independent random 64-lowercase-hex
capability for each real provisioned update. Raw tags and parent tags belong only in
this protected file and native station traffic. Public `SendLocalList` parameters use
`payload_schema = "urn:uob:ocpp16:SendLocalListReference:1"` and only
`{"listVersion":17,"updateType":"Full","updateReference":"list16:…"}`
through the existing authenticated `POST /api/v1/commands` envelope. Target the exact
station-root resource, not a connector. `GetLocalListVersion` and `ClearCache` use the
unchanged OCPP 1.6 request URNs and `{}` payloads through the same privileged route.

Provisioning is bounded to 128 updates, 256 entries per update, 64 KiB per native
update, 1 MiB retained content plus metadata, and a 2 MiB startup file. Native tags and
parent tags permit up to 20 Unicode characters; original spelling is preserved.
Full entries require `idTagInfo`; a Differential entry without it deletes that tag.
Absent/empty Full clears the list. Absent/empty Differential changes only the stored
update version. Versions are signed native `i32`; update versions `-1` and `0` are
invalid, while query `0` means empty and `-1` means unsupported. Known
`SendLocalListMaxLength` bounds every update, and known `LocalAuthListMaxLength`
bounds Full entry count; unknown limits remain unknown.

The provider rechecks exact scope, capability expiry/revocation, native limits,
active connection generation and command deadline before first send polling.
Accepted Send evidence records the requested version, not independently observed
contents or offline usability. ClearCache affects the authorization cache, not the
local list. Lost/malformed/unpaired replies remain uncertain and are never implicitly
replayed. A later version query does not prove contents or settle that uncertainty;
an explicit fresh Full update is the conservative resynchronization.

Stop the daemon, atomically replace the private file with fresh capabilities and
restart to replace provisioning. There is no hot reload or public revocation endpoint.
See [authorization boundaries](../security/local-authorization.md) and the
[independent simulator scenarios](../simulator/scenario-runner.md) for actual offline,
disk recovery and native Reset verification.

### Protected OCPP 2.0.1 station authorization list and cache

Use the same charging settings and independently default-off station options as
above, but select `protocol = "ocpp201"` for that exact roster station. Keep
separate reader/control/privileged grants and the existing protected directories.
An enabled 1.6 station and enabled 201 station may share this startup file, but their
native entries and capability prefixes route to separately typed providers.

The owner-only file uses the unchanged entry shape, for example:

```json
{
  "updates": [{
    "station_id": "demo-201",
    "update_reference": "list201:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "expires_at": "2099-01-01T00:00:00Z",
    "request": {
      "versionNumber": 7,
      "updateType": "Full",
      "localAuthorizationList": [{
        "idToken": {"idToken": "native-demo", "type": "Central"},
        "idTokenInfo": {
          "status": "Accepted",
          "cacheExpiryDateTime": "2099-01-01T00:00:00.123Z",
          "chargingPriority": 0,
          "language1": "en",
          "evseId": [1]
        }
      }]
    }
  }]
}
```

Generate independent random capabilities rather than reuse this example.
The public privileged envelope has `protocol:"ocpp201"`, action SendLocalList,
`payload_schema:"urn:uob:ocpp201:SendLocalListReference:1"` and only
`{"versionNumber":7,"updateType":"Full","updateReference":"list201:…"}`
at the exact station root. Query/Clear use native
`urn:OCPP:Cp:2:2020:3:GetLocalListVersionRequest` and
`urn:OCPP:Cp:2:2020:3:ClearCacheRequest` with `{}` payloads.
Raw entries, group/additional identifiers, personal messages and vendor metadata
belong only in the private file and native wire traffic.

201 update versions are positive i32; query is nonnegative i32 with zero for no
installed list, disabled or uninitialized state. Full can replace a higher version;
Differential must advance at the station. Omitted Full contents empty the list but
retain the submitted positive version, and omitted Differential contents leave
contents unchanged while advancing version. Explicit `[]` is invalid and is not
normalized to omission. Entry without idTokenInfo represents deletion.
ClearCache removes only station authorization cache, not the list or CSMS allowlist.
Accepted ACK and later query do not establish installed contents or actual offline use.

Metadata validation includes native identifierString ASCII spelling (letters,
digits and `* - _ = : + | @ .`, up to36), exact token type, all ten authorization
statuses, chargingPriority[-9,9], positive-i32 EVSE scope, bounded RFC5646
language tags and RFC3339 expiry with at most three fractional digits.
NoAuthorization requires an empty token; other types have no invented nonempty rule.
Valid generic UTF-8 metadata remains inert/private. The service does not implement
tariffs, display messages or an installed-list ledger.
Full entries require idTokenInfo. Only Differential omission of idTokenInfo is a
deletion; omitted whole Full lists still clear while retaining the positive version.

All previous protected caps remain:128 capabilities,256 entries,64KiB **complete
encoded CALL**,1MiB retained native material plus metadata and2MiB startup file.
Explicit current-generation device-model results may reduce limits using exact
LocalAuthListCtrlr ItemsPerMessage/BytesPerMessage/Entries/Enabled/Available/
SupportsExpiryDateTime without instances. Entries Actual is a count, including0;
Integer maxLimit separately bounds Full entries and Differential unique upserts.
Differential deletions do not consume that capacity, but still count toward
ItemsPerMessage and the complete-CALL byte limit. No final list size is inferred.
AuthCacheCtrlr Enabled is separate. Reconnect discards learned facts; no automatic
probing or retry occurs.

Safe result v1.10 local_authorization_201 carries query version or requested Send
version/type and Accepted/Failed/VersionMismatch, or Clear Accepted/Rejected.
Native statusInfo and customData never become public evidence. Lost/malformed/
unpaired replies are uncertain and never automatically replayed or replaced by Full.
Restart uses the existing durable command recovery and historical duplicate result.
SQLite remains v14; new nested public export schemas are v1.11 and runtime revision10.
These are software behavior boundaries, not hardware or certification evidence.

Executable daemon smoke (after the integrated tree is built):

```text
cargo test --locked -p uob-service --test local_authorization201 -- --nocapture
cargo test --locked -p uob-service --test local_authorization16 -- --nocapture
```

The retained 201 suite creates synthetic mode0600 provisioning and starts the actual
daemon for authenticated admission, native wire/results, SQLite/history and
disconnect/restart non-replay. For an independent native station, copy
`bins/uob-sim/examples/local-authorization-2.0.1-config.toml` to a private directory,
configure its real service endpoint and credential file, and run:

```text
target/debug/uob-sim run --config /absolute/private/sim.toml --scenario bins/uob-sim/examples/local-authorization-2.0.1.toml --format jsonl
```

Its management windows require explicit authorized Full/Differential/Clear commands
using protected provider contents that match the scenario's native fixtures.
Joint native station/offline assertions are separate from daemon ACK/result assertions.
These are commands to execute during integration, not claims of completed checks.


The console requires a fresh destination/station confirmation and the appropriate independent
credential for every command submission. Expired, malformed, out-of-scope and unsupported
requests are rejected. An HTTP 202 records admission, not native acceptance or physical effect.
Inspect the station-scoped sanitized command history or detail by request ID before deciding
whether to intentionally retry the **same request with identical content and authenticated
origin**; an exact retained duplicate does not redispatch, while changed content under the same ID conflicts.
History/detail distinguish admission, dispatch, protocol response and later linked
observed effects. Event IDs connect
observations to the station/resource stream; the correlation link searches the separately
retained, possibly incomplete Debug timeline. A pending native transaction can receive a
transaction-bound `TxProfile` charging limit without proof of power flow; even an accepted
profile is not observed charging success. Start/stop/availability observations appear only
after later station reports, not merely because the protocol replied `Accepted`.

The optional plaintext demo listener and its local grants do **not** expose charging ingress
or command authority in production.

### Protected OCPP 1.6 reservations

`reserve_now` and `cancel_reservation` are independent, default-off, privileged
**demo-only** station options for `protocol = "ocpp16j"`. `reserve_now` also requires a
per-station `reservation16_file`; `reserve_connector_zero_supported` separately offers
`ReserveNow` on connector `0` (any connector) and is never inferred from the station.
`reservation16_file` and `reserve_connector_zero_supported` are rejected for an OCPP 2.0.1
station; 2.0.1 reservations use their own options below.

```toml
[[charging.stations]]
id = "demo-1"
protocol = "ocpp16j"
credential_file = "/srv/uob-demo/private/demo-1.credential"
reservation16_file = "/srv/uob-demo/private/demo-1-reservations.json"
reserve_now = true
cancel_reservation = true
reserve_connector_zero_supported = false
```

The startup-only provider file is service-owned mode `0600`, under a protected
owner-only directory, at most 64 KiB, and holds at most 256 reservation and identity
entries combined. Unknown fields, duplicate references and duplicate identities are
rejected without echoing their contents. Its exact shape is:

```json
{
  "reservations": [{
    "reference": "reserve16:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "request": {
      "connectorId": 1,
      "expiryDate": "2099-01-01T00:00:00Z",
      "idTag": "demo-native-tag",
      "parentIdTag": "demo-native-group",
      "reservationId": -113
    },
    "expires_at": "2099-01-01T00:00:00Z",
    "revoked": false
  }],
  "identities": [{
    "idTag": "demo-member-tag",
    "parentIdTag": "demo-native-group",
    "authorize": true,
    "policy_revision": 1
  }]
}
```

Generate an independent random 64-hex reference per provisioned reservation. Raw
`idTag`/`parentIdTag` values (at most 20 Unicode characters) belong only in this file
and native station traffic; matching uses one-way case-folded keys. Public `ReserveNow`
uses `payload_schema = "urn:uob:ocpp16:ReserveNowReference:1"` and only
`{"connectorId":1,"expiryDate":"…","reservationId":-113,"reservationReference":"reserve16:…"}`
through the existing authenticated `POST /api/v1/commands` envelope. A positive
connector must target that exact connector resource with its OCPP 1.6
`native_protocol_reference`; connector `0` targets the station root. `CancelReservation`
uses the unchanged OCPP 1.6 request URN with only `{"reservationId":…}` and always
targets the station root.

Reservation IDs keep the full signed native `i32` range. A same-ID `ReserveNow`
replaces the previous reservation only once the station accepts it; a rejection keeps
the previous owner. Identity entries are native group facts: `parentIdTag` is returned
on an accepted `Authorize` and lets a member's real `StartTransaction` consume a
reservation made for the same parent. A parent tag never authorizes as a token, and
`authorize`/`policy_revision` stay separate central policy, so a later revocation
denies the start while still recording that it consumed the reservation.

Durable reservation state reconciles native statuses with independent station
facts: inclusive expiry (also enforced while the station is offline), Faulted or
Unavailable status, accepted cancellation and a real matching start each terminate
it. Lost, malformed or late replies stay uncertain and are never replayed after
restart; a late `Accepted` reply is kept as evidence but never revives an expired or
superseded reservation. This is software boundary evidence, not hardware
interoperability or OCA certification.

### Protected OCPP 1.6 firmware updates

`update_firmware` (OCPP 1.6 `UpdateFirmware`) and `signed_update_firmware` (Security
Whitepaper Edition 4 `SignedUpdateFirmware`) are privileged, default-off,
**demo-only** station options for `protocol = "ocpp16j"`. A station gets at most one of
them: a signed-only charger answers the original message with `NotSupported` (L01.FR.20).
Each firmware station also needs `firmware_job_timeout_seconds` (60–604800). Using
either option requires one `[charging.firmware]` section with a `catalog_file`, and that
section is rejected when no station uses it.

```toml
# Alongside the existing [charging] section:
[charging.firmware]
listen_addr = "127.0.0.1:9100"           # loopback test artifact service
public_base = "http://127.0.0.1:9100"    # optional; defaults to http://<listen_addr>
spool_directory = "/srv/uob-demo/artifact-spool"
catalog_file = "/srv/uob-demo/private/firmware-catalog.json"
manufacturer_root_file = "/srv/uob-demo/public/manufacturer-root.pem"  # optional output
organization = "Demo CSO"                # optional test PKI organization

[[charging.stations]]
id = "demo-1"
protocol = "ocpp16j"
credential_file = "/srv/uob-demo/private/demo-1.credential"
signed_update_firmware = true            # or update_firmware = true
firmware_job_timeout_seconds = 3600
```

Requirements for the files:

- `spool_directory` is a private `0700` directory.
- The startup-only catalog is service-owned mode `0600`, at most 64 KiB, and lists 1–16
  unique references:

  ```json
  {"artifacts": [
    {"reference": "station-fw-2.0-signed.bin", "file": "/srv/uob-demo/private/fw-2.0.bin", "signed": true},
    {"reference": "station-fw-1.1.bin", "file": "/srv/uob-demo/private/fw-1.1.bin", "signed": false}
  ]}
  ```

- Each image is an owner-only `0600` file of at most 32 MiB.

At startup the service:

- generates a fresh `TEST ONLY` PKI;
- signs every `signed` image over its complete bytes with RSA-PSS SHA-256;
- serves the images at `{public_base}/artifacts/{reference}`;
- when configured, writes the generated manufacturer root PEM so a demo station can trust
  it.

Restarting regenerates the PKI and republishes the images. Every resulting artifact is
marked `test_only`, and both providers refuse production environments.

Commands go through the existing authenticated `POST /api/v1/commands` envelope and
target the station root only. They name a catalog reference, never a location:

```json
{"request_id":"fw-1","resource":{"bridge_id":"local-demo","station_id":"demo-1"},
 "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp16j","action":"SignedUpdateFirmware",
   "payload_schema":"urn:uob:ocpp16:SignedUpdateFirmwareReference:1",
   "payload":{"requestId":122,"artifactReference":"station-fw-2.0-signed.bin",
     "retrieveDateTime":"2026-10-06T12:00:00Z","installDateTime":"2026-10-06T12:05:00Z",
     "retries":3,"retryInterval":60}}},
 "expires_at":"2026-10-06T12:10:00Z"}
```

The legacy action uses `urn:uob:ocpp16:UpdateFirmwareReference:1` with
`artifactReference`, `retrieveDate` and optional `retries`/`retryInterval`.

Before sending, the bridge resolves the reference, checks that the artifact kind matches the
action, and verifies the signing certificate against the test manufacturer root. Any refusal
is reported as not sent.

The command result's `firmware_16` shows:

- the sent artifact's SHA-256 and size;
- the exact native reply;
- the durable job, which advances as the station reports `FirmwareStatusNotification` or
  `SignedFirmwareStatusNotification`.

A job that misses its deadline becomes `timed_out` but keeps blocking a release drain until
the station reports an end state. To resolve it, send a `TriggerMessage` for
`FirmwareStatusNotification`; a station that is not busy answers `Idle`. A lost reply stays
`uncertain` and is never resent after a restart. See
[OCPP 1.6 firmware](../architecture/ocpp16-firmware.md) for the state rules.

### Protected OCPP 1.6 diagnostics and log uploads

`get_diagnostics` (OCPP 1.6 `GetDiagnostics`) and `get_log` (Security Whitepaper Edition 4
`GetLog` for a diagnostics or security log) are privileged, default-off, **demo-only** station
options for `protocol = "ocpp16j"`. They are independent of each other and of firmware. A
station with either one needs `diagnostics_job_timeout_seconds` (60–86400) and may lower the
upload cap with `diagnostics_upload_max_bytes` (1 to 33554432, default 8 MiB).

Stations upload to the same loopback test artifact service as firmware, so either option
requires a `[charging.firmware]` section. Without a firmware station the section has no
`catalog_file`:

```toml
[charging.firmware]
listen_addr = "127.0.0.1:9100"
spool_directory = "/srv/uob-demo/artifact-spool"

[[charging.stations]]
id = "demo-1"
protocol = "ocpp16j"
credential_file = "/srv/uob-demo/private/demo-1.credential"
get_diagnostics = true
get_log = true
diagnostics_job_timeout_seconds = 1800
```

Commands target the station root and never carry a location; the bridge opens a fresh
destination for each one:

```json
{"request_id":"log-1","resource":{"bridge_id":"local-demo","station_id":"demo-1"},
 "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp16j","action":"GetLog",
   "payload_schema":"urn:uob:ocpp16:GetLogReference:1",
   "payload":{"logType":"SecurityLog","requestId":124,
     "oldestTimestamp":"2026-10-01T00:00:00Z","retries":2,"retryInterval":30}}},
 "expires_at":"2026-10-07T12:10:00Z"}
```

`GetDiagnostics` uses `urn:uob:ocpp16:GetDiagnosticsReference:1` with optional `startTime`,
`stopTime`, `retries` and `retryInterval`.

The command result's `diagnostics_16` shows:

- the offered destination's log type, byte cap and `test_only` marking;
- the exact native reply, including the station's file name;
- the durable job, which advances as the station reports `DiagnosticsStatusNotification` or
  `LogStatusNotification`.

When the station reports `Uploaded`, the job becomes `uploaded` only if the artifact service
holds a complete file for it. The result then shows that file's SHA-256 and size; otherwise the
job is `upload_unconfirmed`. Received files live in unlinked spool files for the life of the
process; this is not log storage.

A job that misses its deadline becomes `timed_out` but keeps blocking a release drain. A
`TriggerMessage` for `DiagnosticsStatusNotification` makes a station that is not busy answer
`Idle`, which resolves a stalled legacy job. A lost reply stays `uncertain` and is never resent
after a restart. See [OCPP 1.6 diagnostics](../architecture/ocpp16-diagnostics.md) for the state
rules.

### Protected OCPP 2.0.1 reservations

For `protocol = "ocpp201"`, the same independent, default-off, privileged **demo-only**
`reserve_now` and `cancel_reservation` options apply. `reserve_now` requires a
per-station `reservation201_file`. A reservation without an `evseId` (an unspecified
EVSE) is offered only when `reserve_non_evse_specific_supported = true`, which mirrors the
station's `ReservationCtrlr.NonEvseSpecific` and is never inferred from it.
`reservation201_file` and `reserve_non_evse_specific_supported` are rejected for an OCPP
1.6 station.

```toml
[[charging.stations]]
id = "demo-201"
protocol = "ocpp201"
credential_file = "/srv/uob-demo/private/demo-201.credential"
reservation201_file = "/srv/uob-demo/private/demo-201-reservations.json"
reserve_now = true
cancel_reservation = true
reserve_non_evse_specific_supported = false

[[charging.stations.resources]]
evse_id = "one"
native_evse_id = 1
```

`ReserveNow` with an `evseId` is offered on, and must target, the exact EVSE resource
(an EVSE entry without a connector). The provider file follows the 1.6 file rules
(service-owned `0600` under an owner-only directory, at most 64 KiB and 256 entries,
unknown fields and duplicates rejected without echo). Requests must be valid against the
pinned OCPP 2.0.1 `ReserveNowRequest` schema; vendor `customData` and `NoAuthorization`
tokens are rejected:

```json
{
  "reservations": [{
    "reference": "reserve201:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "request": {
      "id": -114,
      "expiryDateTime": "2099-01-01T00:00:00Z",
      "evseId": 1,
      "connectorType": "cType2",
      "idToken": {"idToken": "demo-native-token", "type": "ISO14443"},
      "groupIdToken": {"idToken": "demo-native-group", "type": "Central"}
    },
    "expires_at": "2099-01-01T00:00:00Z",
    "revoked": false
  }],
  "identities": [{
    "idToken": {"idToken": "demo-member-token", "type": "ISO14443"},
    "groupIdToken": {"idToken": "demo-native-group", "type": "Central"},
    "authorize": true,
    "policy_revision": 1
  }]
}
```

Public `ReserveNow` uses `payload_schema = "urn:uob:ocpp201:ReserveNowReference:1"` and
only `{"id":-114,"expiryDateTime":"…","evseId":1,"connectorType":"cType2","reservationReference":"reserve201:…"}`
(`evseId` and `connectorType` are optional and must match the provisioned request).
`CancelReservation` uses `urn:OCPP:Cp:2:2020:3:CancelReservationRequest` with only
`{"reservationId":…}` and always targets the station root. Raw `idToken`/`groupIdToken`
values stay in this file and native traffic; matching uses one-way keys of the token type
plus the case-folded idToken. Identity entries give the bridge native group membership for
attribution and optional explicit `Authorize` policy; group linkage never authorizes.

Durable 2.0.1 reservation state reconciles native statuses with explicit station facts:
`ReservationStatusUpdate` `Expired` or `Removed` (acknowledged only after commit), a
`TransactionEvent` `reservationId` on the reserved EVSE whose idToken, when present,
matches the reservation or a provisioned group member, accepted cancellation, and
inclusive trusted expiry while offline. The bridge never infers `Removed` from a
`StatusNotification`; a same-ID update while a replacement is in flight is recorded as
ambiguous. Native `statusInfo` is validated and not retained. Lost, malformed, CALLERROR
and late replies stay uncertain and are never replayed after restart. This is software
boundary evidence, not hardware interoperability or OCA certification.

### Protected OCPP 2.0.1 firmware updates

For `protocol = "ocpp201"`, `update_firmware` enables OCPP 2.0.1 `UpdateFirmware`. It is
privileged, default-off and **demo-only**, needs `firmware_job_timeout_seconds` (60–604800)
and the same `[charging.firmware]` section as
[the OCPP 1.6 firmware updates](#protected-ocpp-16-firmware-updates). OCPP 2.0.1 has one
message for both security modes, so the mode is set per station:

- by default the station receives a **secure** update (L01) with the signing certificate and
  signature, and only `signed` catalog images can be sent to it;
- `non_secure_firmware = true` selects a **non-secure** update (L02) without either, and only
  unsigned images can be sent to it.

```toml
[[charging.stations]]
id = "demo-201"
protocol = "ocpp201"
credential_file = "/srv/uob-demo/private/demo-201.credential"
update_firmware = true
# non_secure_firmware = true             # L02 instead of L01
firmware_job_timeout_seconds = 3600
```

`signed_update_firmware` is refused for 2.0.1 stations. Commands target the station root and
name a catalog reference and the native `requestId`:

```json
{"request_id":"fw-201-1","resource":{"bridge_id":"local-demo","station_id":"demo-201"},
 "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":"UpdateFirmware",
   "payload_schema":"urn:uob:ocpp201:UpdateFirmwareReference:1",
   "payload":{"requestId":123,"artifactReference":"station-fw-2.0-signed.bin",
     "retrieveDateTime":"2026-10-07T12:00:00Z","installDateTime":"2026-10-07T12:05:00Z",
     "retries":3,"retryInterval":60}}},
 "expires_at":"2026-10-07T12:10:00Z"}
```

Before sending a secure update, the bridge verifies the signing certificate against the test
manufacturer root. A wrong artifact kind, an unknown reference or an untrusted certificate is
reported as not sent. A `requestId` still retained for the station is refused, because the
station's `FirmwareStatusNotification` reports are matched by it alone.

The command result's `firmware_201` shows the native `request_id`, whether the update was
`secure`, the sent artifact's SHA-256 and size, the exact native reply (with its reason code
but without `additionalInfo`), and the durable job. As with OCPP 1.6, a job that misses its
deadline becomes `timed_out` but keeps blocking a release drain. A `TriggerMessage` for
`FirmwareStatusNotification` makes a station report `Idle` once it has finished, which
settles the job as `station_idle`. See [OCPP 2.0.1 firmware](../architecture/ocpp201-firmware.md)
for the state rules.

### Protected OCPP 2.0.1 log retrieval

For `protocol = "ocpp201"`, `get_log` enables OCPP 2.0.1 `GetLog` (N01) for a diagnostics or
security log. It is privileged, default-off and **demo-only**, needs
`diagnostics_job_timeout_seconds` (60–86400) and accepts the same optional
`diagnostics_upload_max_bytes` (1 to 33554432, default 8 MiB) as
[the OCPP 1.6 log uploads](#protected-ocpp-16-diagnostics-and-log-uploads). OCPP 2.0.1 has no
`GetDiagnostics`, so `get_diagnostics` is refused for these stations. Uploads go to the same
`[charging.firmware]` artifact service, which needs no `catalog_file` unless a station also
enables firmware.

```toml
[charging.firmware]
listen_addr = "127.0.0.1:9100"
spool_directory = "/srv/uob-demo/artifact-spool"

[[charging.stations]]
id = "demo-201"
protocol = "ocpp201"
credential_file = "/srv/uob-demo/private/demo-201.credential"
get_log = true
diagnostics_job_timeout_seconds = 1800
```

Commands target the station root and never carry a location; the bridge opens a fresh
destination for each one:

```json
{"request_id":"log-201-1","resource":{"bridge_id":"local-demo","station_id":"demo-201"},
 "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":"GetLog",
   "payload_schema":"urn:uob:ocpp201:GetLogReference:1",
   "payload":{"logType":"SecurityLog","requestId":124,
     "oldestTimestamp":"2026-10-01T00:00:00Z","retries":2,"retryInterval":30}}},
 "expires_at":"2026-10-07T12:10:00Z"}
```

A `requestId` still retained for the station is refused, because the station's
`LogStatusNotification` reports are matched by it alone. Sending another `GetLog` while an
upload is running lets the station cancel the first one (`AcceptedCanceled`).

The command result's `diagnostics_201` shows:

- the offered destination's log type, byte cap and `test_only` marking;
- the exact native reply, including the station's file name and reason code;
- the durable job, which advances as the station reports `LogStatusNotification`.

When the station reports `Uploaded`, the job becomes `uploaded` only if the artifact service
holds a complete file for it. The result then shows that file's SHA-256 and size; otherwise the
job is `upload_unconfirmed`. A job that misses its deadline becomes `timed_out` but keeps
blocking a release drain. A `TriggerMessage` for `LogStatusNotification` makes a station that
is not uploading answer `Idle` without a `requestId`; that report is accepted only as the
answer to a pending trigger and settles a stalled job as `station_idle`. A lost reply stays
`uncertain` and is never resent after a restart. See
[OCPP 2.0.1 log retrieval](../architecture/ocpp201-diagnostics.md) for the state rules.

## Commands and exit codes

Validate without binding a socket, resolving DNS, reading credentials, or starting adapters:

```text
uob config check --config bridge.toml
```

Production release preflight can additionally read and validate secret references with
`uob config check --config bridge.toml --secrets`. See
[production preflight](release-preflight-backup.md) for reference restrictions and limits.

Success writes one JSON object to stdout:

```json
{"status":"valid"}
```

Start the service with the optional browser entry asset enabled or disabled:

```text
uob serve --config bridge.toml
uob serve --config bridge.toml --no-ui
```

`--no-ui` removes only the static browser route. Health, identity, metrics, and every management
API route remain mounted. The process does not open a browser in either mode.

Consume the configured management event stream as newline-delimited JSON:

```text
uob events --format jsonl
uob events --config staging.toml --after uob:event:41 --format jsonl
```

Without `--config`, `events` reads `bridge.toml`. Its default endpoint is the configured local
management listener at `/api/v1/events`. An explicit remote endpoint must use HTTPS and provide a
`credentials_file`; the file is read only when `events` connects and its trimmed content is sent
as a bearer credential. Redirects and proxy use are disabled so credentials stay bound to the
validated endpoint. SSE `data` records must be bounded valid JSON and are re-encoded as compact
JSONL. A server response that reflects the bearer value is rejected before it reaches stdout.

Exit code `0` means the requested operation completed successfully. Exit code `2` identifies
invalid arguments or configuration. Exit code `1` identifies runtime, network, stream, or output
failure. Diagnostics use stable sanitized categories and do not reproduce rejected configuration
values, credential contents, response bodies, or filesystem paths.

## Independent release control

Release commands connect directly to the independent supervisor's protected Unix socket.
They do not load `bridge.toml`, contact the management API, or start a bridge process:

```text
uob release stage --bundle /srv/delivery/candidate
uob release qualify --release DIGEST --evidence evidence.json
uob release promote --release DIGEST
uob release rollback --to previous-good
uob release status --format json
uob release events --format jsonl
uob release events --format jsonl --after 41
```

Every form accepts `--socket PATH`; the default is
`/run/uob-release-manager/control.sock`. Use only an administrator-controlled socket.
The supervisor authenticates the process's kernel UID, not a CLI-supplied credential.
Group membership permits connecting but does not confer an operation grant:

| Commands | Required supervisor permission |
|---|---|
| `status`, `events` | `read` |
| `stage`, `qualify` | `stage` |
| `promote`, `rollback` | `activate` |

`DIGEST` is the exact 64-character lowercase SHA-256 artifact identity, not a release label.
`--bundle` takes a directory in the existing
[signed-artifact format](signed-artifact-store.md), not a platform-package tarball.
The CLI reads its bounded `manifest.json` to select the digest. The administrator must first
install the signed manifest, signature and payload with the existing
`uob-release-manager install` command. `stage` asks the supervisor to reverify that installed
candidate's signature, bytes and eligibility; it does not install local files or claim that a
staging workload ran.

`--evidence` identifies exact document bytes, bounded to 64 KiB. Before calling `qualify`, the
administrator or trusted delivery pipeline must provision those bytes and their detached
signature in the supervisor's [protected evidence inbox](release-qualification.md).
The CLI hashes the local file; only its digest crosses IPC. It cannot supply a trusted key,
upload unsigned claims, write the inbox, or override the configured qualification matrix.
Manifest/evidence inputs must be regular files; final-component symlinks and FIFOs are rejected.

The supervisor remains authoritative. Missing or invalid installation/evidence returns policy
errors such as `artifact_rejected`, `evidence_rejected`, or `qualification_required`.
Qualified promotion uses the existing preflight gate and returns `preflight_rejected` or
`activation_blocked` where production admission/process control is unavailable through IPC.
Rollback likewise returns the existing `qualification_required` policy response.
Neither command bypasses compatibility, drain, or activation ownership. These CLI forms do not
turn the separate internal activation/automatic-rollback APIs into unconditional operator actions.

Status and mutation commands emit one JSON response containing `protocol`, `manager_version`,
and `code`, with status evidence where available. Supervisor policy failures retain their
machine-readable response and exit `1`; they are not reported as success.
Local failures emit `{"error":"..."}` with a stable sanitized category. Invalid arguments exit
`2`; input, transport, protocol and output failures exit `1`. Diagnostics remain on stderr.
Connect/write waits are bounded to one second each; the absolute response deadline is six
minutes, allowing the supervisor's configured preflight to take up to five minutes.
A timeout does not cancel supervisor work. Inspect status/events before deciding whether to retry.

Release events are a finite snapshot, not the management SSE stream or a follow mode. Each
retained record is one JSON line with `sequence`, `uid`, digest-only `request`,
`result`, and `actor` (`operator` or `supervisor`). Internal decisions also carry
safe typed `decision` evidence for compatibility/drain, health and rollback.
Operator UIDs come from socket peer credentials; internal decisions use the
supervisor's effective UID. The final line is:

```json
{"type":"metadata","cursor":42,"truncated":false,"oldest_sequence":1,"latest_sequence":42}
```

`--after` is an exclusive unsigned sequence cursor covering both actors. Retention
is at most 64 records and may be lower to keep the combined state below 64 KiB.
`truncated:true` means the requested cursor predates available history; archive output
externally if a longer history is required. Current incident context remains pinned
in status independently of event eviction. Older records without an actor are
operator records. Reads do not change the ledger. Unauthorized/malformed requests
rejected before recording are not audit entries.

Run `./scripts/test-release-cli.sh` for real CLI-to-supervisor checks with no bridge/API running,
including read-only permissions, untrusted evidence, candidate selection, restart,
cursor retrieval, and internal audit export after automatic rollback and bridge death.
It builds `uob` separately and runs the cross-binary regression cases; the workspace
verifier invokes it automatically.
