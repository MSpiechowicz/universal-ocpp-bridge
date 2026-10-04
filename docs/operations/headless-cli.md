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
privileged credential authorizes the query. An OCPP 2.0.1 opt-in is rejected, as is an opt-in
station with any configured native connector ID above `i32::MAX` (2147483647). Default-off
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
This opt-in does not implement OCPP 2.0.1 schedules, simulator smart charging or certification.

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
