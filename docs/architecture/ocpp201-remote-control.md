# OCPP 2.0.1 remote control

`v201::remote_control::RemoteControlSession` implements the existing application
`StationCommandPort` for one authenticated OCPP 2.0.1 socket. Compose it with the ordinary durable
`CommandCoordinator` and scoped access guard. Start and stop require control permission; native
Reset, UnlockConnector, full native Set/ClearChargingProfile and protected
SetVariables/SetNetworkProfile require privileged control.
There is no additional HTTP endpoint or authorization path. `uob serve` composes this
boundary only in the independently credentialed, loopback, demo-only charging host.

The owner supplies committed registration, topology, capabilities, availability and transaction
state. Updates cannot change station identity or connection epoch, or move observation time
backward. Reconnect requires a new port. Old commands are never queued for a replacement socket.

| Operation | Canonical request | Native behavior |
|---|---|---|
| RequestStartTransaction | Start with a locally authorized reference; station scope or explicit EVSE scope | Optional evseId, a durable remoteStartId, typed idToken; Accepted/Rejected and optional native transactionId |
| RequestStopTransaction | Stop with a retained canonical transaction ID in the addressed station, EVSE or connector | Native transactionId from committed protocol evidence; Accepted/Rejected |
| Reset | Privileged Ocpp with `urn:OCPP:Cp:2:2020:3:ResetRequest`, Immediate or OnIdle | Station scope omits evseId; EVSE scope must match it exactly; Accepted/Rejected/Scheduled |
| UnlockConnector | Privileged Ocpp with `urn:OCPP:Cp:2:2020:3:UnlockConnectorRequest` | Both positive EVSE and connector IDs must match the addressed connector; Unlocked/UnlockFailed/OngoingAuthorizedTransaction/UnknownConnector |
| SetChargingProfile | Privileged Ocpp with `urn:OCPP:Cp:2:2020:3:SetChargingProfileRequest` | Station0 or exact positive EVSE; full request, Accepted/Rejected |
| ClearChargingProfile | Privileged Ocpp with `urn:OCPP:Cp:2:2020:3:ClearChargingProfileRequest` | ID alone or nonempty AND criteria; Accepted/Unknown, no removed IDs |
| SetVariables | Privileged Ocpp with `urn:uob:ocpp201:SetVariablesReference:1` | Exact bound references resolve to native attribute values only at send; all six independent native statuses |
| SetNetworkProfile | Privileged Ocpp with `urn:uob:ocpp201:SetNetworkProfileReference:1` | Station-only signed configurationSlot and private full connectionData; Accepted/Rejected/Failed, Accepted means staged |
| UpdateFirmware | Privileged Ocpp with `urn:uob:ocpp201:UpdateFirmwareReference:1` | Station root only; a provider artifact matching the station's secure/non-secure mode and a durable job before dispatch (see [OCPP 2.0.1 firmware](ocpp201-firmware.md)); Accepted/Rejected/AcceptedCanceled/InvalidCertificate/RevokedCertificate |

A start cannot select a connector on the wire. Connector-scoped starts fail explicitly rather
than widening permission to their EVSE. Station-scoped starts omit evseId and require station-scoped
local authorization. The service conservatively refuses starting on an EVSE with a retained live
transaction; it does not infer from Pending alone that an existing transaction is unauthorised.
Unavailable resources, missing capabilities, unknown payload fields/schemas, unsupported native
operations and violated advertised parameters fail closed. Charging profiles embedded in a
remote-start request and group tokens remain unsupported; native SetChargingProfile is a
separate opt-in described below and cannot bypass the normal start guard.

`LocalRemoteStartIdentity::new` resolves at most 128 bounded typed tokens at startup through the
configured trusted identity provider, with a one-second bound per resolution. It rechecks the
persisted local authorization policy, exact resource scope, expiry and revocation at dispatch.
The default typed local provider maintains token namespaces and case normalization; raw token
material is never written to command storage. Additional/certificate evidence requires a separate
provider workflow and is not accepted by this remote-start cache. Charger-originated authorization
remains independent of the remote response and the charger's AuthorizeRemoteStart setting.

## Durable correlation and native response evidence

The application-owned `RemoteControlStore` uses the same bounded SQLite worker as operational
commands. Schema v7 adds a monotonic remote-start counter and one evidence row per retained
command. Reservation and row creation commit together before socket dispatch. Concurrent
reservations return the same value, the counter never reuses an allocated ID, and exhaustion of the
positive i32 range fails closed. Evidence rows follow command retention through a foreign-key
cascade; unresolved commands retain their evidence. The counter survives pruning. Release drain
blocks new reservations and sealed-boundary writes.

The port persists only the validated native response status and optional transactionId. In
particular, a Reset `Scheduled` response remains distinguishable from `Accepted` in this evidence,
although both represent protocol acceptance in the canonical result. StatusInfo free text and
vendor payloads are not retained. Evidence is committed before the coordinator stores its result;
a crash between those writes leaves conservative uncertain-command recovery, never retransmission.
A reply's native transactionId does not create or activate a transaction.

OCPP 2.0.1 TransactionEvent decoding retains remoteStartId in application observations and durable
transaction protocol state. Later messages that omit it retain the existing value; conflicting
changes fail before snapshot mutation. The optional field is published in the additive v1.1
station-snapshot and export schemas; released v1.0 schemas are preserved. Existing binaries require
explicit schema-v7 rollback qualification; this change does not claim old-to-new-to-old evidence.

The host reads committed events and native evidence, then calls
`remote_control::observation::transaction_effect` and the existing coordinator reconciliation
method after dispatch finishes. Starts match remoteStartId, resource, time, RemoteStart trigger
and the response transactionId when supplied. Both Started and Updated events can carry the link.
Stops match the canonical transaction and a later committed Ended event. Duplicate event links are
idempotent. Reset/unlock never fabricate physical success from a response, stop or reconnect.

Timeouts, malformed replies, lost sockets and failed evidence persistence remain uncertain.
Expiry is rechecked after allocation and before bounded enqueue, with the existing socket-owned
last-send deadline. Late replies cannot rewrite durable results. Identical retries return retained
results without sending, including after process loss or reconnect.

## Opt-in TriggerMessage

`TriggerMessage` uses the same privileged, per-request authenticated demo command
path and requires the station's `trigger_message = true` opt-in, the advertised
resource capability and pinned `urn:OCPP:Cp:2:2020:3:TriggerMessageRequest`
schema. It supports exactly eleven native `requestedMessage` values:
`BootNotification`, `LogStatusNotification`, `FirmwareStatusNotification`,
`Heartbeat`, `MeterValues`, `SignChargingStationCertificate`,
`SignV2GCertificate`, `StatusNotification`, `TransactionEvent`,
`SignCombinedCertificate` and `PublishFirmwareStatusNotification`. Unlike OCPP
1.6, the payload uses an optional `evse` object (`id`, optional `connectorId`),
not a flat connector ID. Station-only Boot, log, firmware, heartbeat, charging
station certificate and publish-firmware-status classes require a station
resource; an irrelevant supplied EVSE does not turn them into EVSE requests.
Boot is permitted only before Accepted registration. StatusNotification needs
an exact existing EVSE **and** connector; a missing connector is not widened to
the EVSE. MeterValues needs station or EVSE permission, never connector-only
permission, since its later report identifies the EVSE, not the connector.
TransactionEvent may address an EVSE or connector; its later report must have
`Trigger` reason. V2G/combined signing may address station or EVSE, but
SignCertificate receipts carry no trusted EVSE identity.

Omitting `evse` for applicable classes requests all configured EVSEs on the
station and freezes that sorted, deduplicated target set before dispatch.
Empty sets and more than 64 EVSEs fail closed. The exact native reply
`Accepted`, `Rejected` or `NotImplemented` (and optional `statusInfo`) is
persisted independently from subsequent committed, compatible station CALLs.
At dispatch the coordinator fixes a 60-second observation deadline. Durable
`pending`, `partial`, `observed`, `absent`, `unsupported` or `unattributable`
status reflects separate observations, not causal proof of the trigger. An
EVSE-scoped certificate's station-level receipt is `unattributable`, never
EVSE completion. Native acceptance does not prove a later call, successful
certificate workflow or physical charging.

Schema v11 indexes pending 2.0.1 trigger results and station events for bounded
reconciliation. It preserves the original request edition, class, scope,
dispatch time and event IDs; terminal deadlines are finalized once. Denied or
malformed requests send nothing; delayed replies, lost sockets and process
restarts cannot rewrite results or replay an uncertain command. Simulator
certificate triggers return `NotImplemented`: it has no private key/CSR
generator and does not fabricate a signing request. Full certificate-chain,
CSR issuance and ISO 15118 workflows remain separate planned features.

## Opt-in native charging profiles

`set_charging_profile` and `clear_charging_profile` independently default to `false` in each
OCPP 2.0.1 station table. Neither enables canonical charging limits, GetVariables, target
privileged ingress or production charging. Native Set/Clear requires the exact advertised
station or EVSE resource and a privileged grant. Connector-only permission never widens to
EVSE authority. Set's `evseId=0` owns a station profile; positive IDs require the configured
EVSE-only resource. ID clears and criteria without a positive EVSE require station authority.
For Clear, omitted EVSE means all EVSEs, while zero means station-owned profiles only.
Profile IDs are station-global: EVSE/connector-scoped replacement cannot erase another
EVSE's or station0's known/uncertain ownership. Explicit station-privileged ID Clear followed
by exact-EVSE Set is the recovery path for such a move.

The supported non-ISO context has exactly one schedule with 1–1024 strictly increasing,
first-zero periods. Signed i32 profile/schedule IDs, native transaction spelling (at most
36 characters), supplied omissions, validity and anchors, duration, A/W units, exact
nonnegative tenths including zero/minChargingRate, phase count and phase selection survive
in immutable evidence. Rates are never rounded or converted using guessed voltage.
Absolute/Recurring requires startSchedule; Relative prohibits it. Only Recurring has a
required Daily/Weekly recurrencyKind. validFrom is inclusive, validTo exclusive; omitted
bounds remain indefinite, and legal duration-truncated periods remain present. Empty/inverted
supplied windows, unknown fields, customData, salesTariff and ISO multi-schedules are rejected.
ChargingStationMaxProfile requires station0 and cannot be Relative. TxProfile requires a
unique established ongoing transaction on its exact EVSE, observed in this connection
generation and rechecked at dispatch. External constraints cannot be installed or cleared
through these commands.

### Explicit destructive baseline and exclusive ownership

With full native Set enabled, a new/upgraded database has **unknown pre-existing profiles**.
Both native Set and canonical SetChargingLimit are blocked until the operator intentionally
submits three station-privileged all-EVSE **purpose-only** Clears:
ChargingStationMaxProfile, TxDefaultProfile and TxProfile. Each valid Accepted or Unknown
acknowledgement establishes that purpose's empty baseline; partial progress persists.
These clears can remove existing charging policies. The bridge never issues them
automatically. This prerequisite assumes exclusive CSMS ownership; the ledger is not an
authoritative discovered inventory. Do not enable full Set where that ownership cannot be
established. Targeted Unknown cannot reconcile uncertain metadata or establish a baseline.
Canonical-only mode with full Set disabled retains its existing eligibility, positive
quantities, stable generated ID, stack0 and phase policy, without a baseline prerequisite.
Its mutations nevertheless reserve and retain their known ownership metadata; they do not
gain the full native result field.
An ordinary canonical charging limit cannot erase a privileged policy sharing its generated ID:
all existing same-ID footprints must exactly match its current transaction, purpose, stack,
scope and validity metadata. Quantity-only repeats remain allowed. Privileged full native
replacement retains its independent within-scope authority. This guard reads durable footprints,
not historical command ownership, and therefore survives pruning.

Reservation JSON contains only bounded generation/baseline/mutation data. Existing external
request IDs remain unchanged in SQL identity columns, not duplicated into its 8 KiB payload.
Footprint scans and transactional selective deletion use compact row IDs rather than copying
potentially large historical owner strings.

SQLite schema14 adds a metadata-only ledger and reservations through the existing atomic
worker. A station holds at most128 footprints, including both old and candidate footprints
after uncertain replacement; there is no eviction. An explicit Clear remains admissible at
the footprint bound. Admission plus reservation commits before durable dispatch-start and
wire. Only one station profile mutation is active; busy/conflict/capacity fails without retry,
while network readers and Heartbeat continue. Accepted Set commits replacement, denial or
proved-not-sent releases only the candidate, uncertainty conservatively retains both.
Accepted Clear removes only matching known metadata without inventing removed IDs.
Purpose-only Accepted/Unknown establishes the explicit baseline. Actual Ended state commits
TxProfile retirement atomically; disconnect, restart and historical pruning never free it.
Ledger rows deliberately have no historical-command foreign-key cascade. Old generations
are fenced before recovery Clear; startup under the exclusive state-directory lock retires
pending reservations conservatively without any replay.
The host fences profile enqueue before a TransactionEvent commit and publishes committed
state before releasing the fence. Snapshot and phase locks cover only bounded enqueue, not
ACK wait. Invalid/duplicate events publish unchanged state and release normally; RAII
cancellation/error cleanup invalidates abandoned generation authority rather than sending
from possibly unpublished state. A fresh reconnect constructs independent authority.
Ended after enqueue but before ACK retires ownership; a later typed Accepted result cannot
reinsert that retired footprint or revise terminal acknowledgement evidence.

Conflict checks apply to every known canonical/native producer: K01.FR06 same-purpose/stack/
EVSE different IDs cannot have overlapping validity; touching endpoints do not overlap.
K01.FR39 prohibits same-stack/transaction TxProfile IDs without an overlap exception.
Station0 versus positive-EVSE TxDefault ownership at the same stack is protected.

### Generation-owned phase proof and results

phaseToUse is allowed only with numberPhases=1, phase1..3, and strictly positive proof from
a correlated, authorized GetVariables Actual result for exact
SmartChargingCtrlr.ACPhaseSwitchingSupported, no component/variable instances, positive
configured EVSE and no connector. Only native string `true` grants proof; false, unknown,
failure, malformed or out-of-order replies revoke it. Issuing a newer matching query revokes
older proof pending its reply. Reconnect/restart clears authority; persisted snapshot
protocol_details and report inventory cannot restore it. GetVariables remains independently
opt-in; no automatic probe occurs. Station0 phase selection fails closed.

Optional action-tagged `CommandResult.charging_profile_201` carries the validated original
request and valid Set Accepted/Rejected or Clear Accepted/Unknown acknowledgement, with only
allowlisted reason codes. CALLERROR, malformed reply, timeout and uncertainty never fabricate
a native status; opaque statusInfo/additionalInfo/customData is not persisted or disclosed.
Terminal results and ledger transitions commit atomically, remain immutable across conflicting
writers and survive reopen. Exact duplicates return retained outcomes without another CALL.
Command-result v1.7 and embedded export-record/export-batch v1.8 retain all historical schema
bytes/routes. Existing payload caps fail explicitly rather than truncating evidence.
A version13 binary rejects database14; this is not automatic downgrade qualification.
Export-schema support is not a new global native-command producer or real PostgreSQL
delivery claim. Native acknowledgement and independent software-peer state are not evidence
of hardware enforcement, OCA certification or physical charging.

Focused verification commands after integration are `cargo test --locked -p
uob-protocol-adapter --test ocpp201_charging_profiles`, `cargo test --locked -p
uob-storage-adapter --test charging_profile201`, and `cargo test --locked -p uob-service --test
charging_profiles201`. The service suite uses separately persisted, independently parsed
software-peer profiles, including replacement, selection, denial, delayed acknowledgement,
Heartbeat, transaction end and separate bridge/peer reopen. These commands are provided for
verification; this section does not claim they have been run or passed.

## Opt-in read-only device-model queries

`GetVariables`, `GetBaseReport` and `GetReport` use the existing authenticated,
privileged one-shot command path. Each OCPP 2.0.1 station table has a separate
`get_variables`, `get_base_report` and `get_report` boolean, all defaulting to
`false`. Enabling one does not enable the others. The advertised resource
capability, current privileged grant and exact native request schema are still
required. The demo-only charging composition restriction remains: these flags
do not expose production charging or grant integration credentials privileged
access. Invalid, unauthorized, unadvertised or out-of-scope requests send no CALL.

| Action | Native payload schema | Query behavior |
|---|---|---|
| GetVariables | `urn:OCPP:Cp:2:2020:3:GetVariablesRequest` | Exact `getVariableData` component/variable identities and optional attribute type |
| GetBaseReport | `urn:OCPP:Cp:2:2020:3:GetBaseReportRequest` | Station-wide `ConfigurationInventory`, `FullInventory` or `SummaryInventory` with native `requestId` |
| GetReport | `urn:OCPP:Cp:2:2020:3:GetReportRequest` | Unchanged native `componentVariable` selectors and `componentCriteria` with native `requestId` |

Unknown request fields and opaque `customData` are rejected, not interpreted as
queries. GetVariables preserves original native names, component/variable
instances, EVSE/connector IDs and attribute types; omitted attribute type means
`Actual`. Full identities match with Unicode case folding without rewriting
preserved fields. Duplicate request identities fail before dispatch. Permuted
results are valid, but missing, extra, duplicate or mismatched result identities
make the native response uncertain; the bridge does not invent `UnknownVariable`
or partial success. Valid partial success consists of independent native
`Accepted`, `Rejected`, `UnknownComponent`, `UnknownVariable` and
`NotSupportedAttributeType` statuses. `Accepted` requires a supplied value,
including a legal explicitly empty string; nonaccepted results have no value.
Presence, emptiness and redaction are separate facts.

GetReport preserves native wildcards for omitted variables/instances and the
OR semantics of component criteria. It does not filter the completed inventory
against the selectors on the host or rewrite the request from configured
topology. Station-wide inventory/wildcards require station-wide authority.
Narrower queries must prove containment using the canonical resource and exact
native EVSE/connector reference; a narrower GetReport requires explicit contained
selectors and no component criteria. Report identities outside the authorized
resource fail collection rather than widen access. Inventory never updates
configured topology or grants.

### Learned request limits and disclosure

Six station request-limit identities disclose positive numeric values: component DeviceDataCtrlr
with no instance/EVSE; variable ItemsPerMessage or BytesPerMessage; variable instance
GetVariables, GetReport or SetVariables; attribute Actual. Validated ASCII-digit values fit
signed 32-bit range. Limits are learned only from explicit authorized queries or valid report
evidence in a bounded connection-generation cache cleared on reconnect.
The additional exact SmartChargingCtrlr.ACPhaseSwitchingSupported EVSE Actual identity discloses only strict
`true`/`false`; only correlated GetVariables replies can authorize phase selection as above.

Unknown station limits remain unknown. A conservative single-entry GetVariables
request can bootstrap them; requests with more than one entry/selector require
both validated item and byte limits for that action. A station-wide GetReport
without selectors remains a native inventory request; GetBaseReport has no
component-variable request list. Known native limits and the local 4,096-entry
and 256 KiB complete-frame bounds are enforced before send. There is no automatic
interrogation, splitting, retry or truncation.

All other values, including unknown/vendor values and WriteOnly attributes,
are redacted before persistence, capture or export. Typed evidence retains
`present`, `empty` and `redacted` flags, supported attribute metadata and safe
characteristics, but omits `statusInfo`, `customData` and
`variableCharacteristics.valuesList`. Validation errors contain no raw payload.
Read-only here means query operations, not permission to disclose every value.

### Native acknowledgement, report completion and recovery

The exact native `Accepted`, `Rejected`, `NotSupported` or `EmptyResultSet`
acknowledgement is independent of collection. `Accepted` alone is not a complete
inventory. `Rejected`/`NotSupported` cannot complete and retain a native-rejection
reason; `EmptyResultSet` means no report is expected. GetVariables also has no
multipart report expectation. Missing acknowledgement cannot become success
merely because all fragments arrived.

Both report actions share the `NotifyReport` namespace. Correlation binds the
authenticated station, actual connection/generation, native signed 32-bit
`requestId` and bridge correlation. Native `requestId` is preserved unchanged,
including negative IDs, and is distinct from the OCPP CALL unique ID.
A budgeted set retires at most 4,096 IDs per connection across both actions;
reuse or exhaustion fails before wire, and only teardown clears the set.

The existing socket owner reserves/registers the route before send.
[Multipart collection](multipart-reports.md) starts its nonrenewing 30-second
deadline at the actual dispatch-start instant, not admission, native ACK or
first fragment. Default bounds are 1 MiB retained item bytes, 4,096 items and
256 fragments, sharing four assembly slots and a 16 MiB queue budget with
protected critical capacity; the complete frame cap is 256 KiB. Ordered items,
attributes, characteristics and each accepted fragment's native `generatedAt`,
`seqNo` and `tbc` are preserved in disclosure-safe evidence. Escaped serialized
device-model output is separately bounded to 1 MiB.

Public report state is `pending`, `complete`, `incomplete` or `not_expected`.
Failure retains accepted fragment/item/byte counts where known and a precise
timeout, limit, capacity, order, correlation, invalid-fragment, disconnect,
interruption, missing-acknowledgement or output reason, never partial inventory.
A valid NotifyReport CALLRESULT acknowledges the wire notification, not durable
inventory completion. Late/unsolicited valid notifications cannot reopen or
replace finalized evidence.

The pending expectation commits with `dispatched` before send. Completed
fragments received before native ACK remain in bounded private staging while
public state stays pending; an Accepted ACK promotes them atomically. ACK-first,
report-first and stale writers merge monotonically without losing the native
reply or replacing terminal report evidence. Collection belongs to the live
connection, not the initiating HTTP request: client timeout or abandonment
does not cancel it. Disconnect/restart terminalizes pending reports without
replay, even if the native command lifecycle already ended. Unavailable recovered
progress stays unknown, not fabricated zero.

SQLite v13 migrates v12 and adds indexed `report_pending` recovery/retention
tracking and private staging. Pending results are protected from pruning until
terminalized. An old v12 binary rejects v13; this migration is not automatic
old-to-new-to-old rollback qualification. Optional `device_model_201` evidence
uses command-result v1.5 and nested export-record/export-batch v1.6, retaining
all released snapshots. Current management/EMS schemas and existing MQTT result
topics carry the additive evidence without widening target ingress privileges
or payload limits.

The read-only #109 addition remains distinct from the protected writes below.
Other device-model writes, monitoring, OCA certification and the complete-release gate
are not implied by either capability.

## Opt-in protected device and network writes

`set_variables` and `set_network_profile` independently default to `false` in each
OCPP 2.0.1 station table. Either opt-in requires the non-secret
`[charging].configuration_values_file` path and existing separate control/privileged
grant files. Only a per-request privileged bearer authorizes the native operation;
the control, read and station credentials do not substitute for it. The ordinary
pinned command registry, advertised resource capabilities, scoped access guard and
durable coordinator remain the only admission/dispatch route. The host stays
loopback and demo-only. Neither write option enables queries, provider/export
delivery, a browser editor or any production charging interface.

### Protected envelopes and immutable ownership

Public SetVariables contains `setVariableData` entries with component, variable,
optional `attributeType` and `valueReference`, **not** `attributeValue`. Public
SetNetworkProfile contains signed 32-bit `configurationSlot` and `profileReference`,
**not** `connectionData`. Their bridge-owned URNs differ from native OCA request
URNs; substituting a native URN, supplying raw material or adding unknown envelope
fields fails closed. Durable command bodies and deferred queues retain references
and metadata only. Result/export evidence never includes a reusable capability.

The startup-only `LocalConfigurationValues201` provider owns values and complete
native profiles. A capability is `cfg201:` plus 64 hexadecimal digits representing
an independently random 256-bit value, never a hash of its content, a counter or
an example copied from documentation. Bind it immutably to the exact ResourceRef,
full component/variable/attribute identity or signed slot, content and expiry.
References compare exactly; native identity comparison uses Unicode case folding
without rewriting preserved names. Omitted attributeType compares as `Actual`.
Instance and EVSE/connector identity remain part of the binding. Duplicate complete
request identities and duplicate installed capabilities are rejected.

SetVariables may address the station or an exactly configured EVSE/connector with
contained native selectors. Connector authority cannot widen to another connector
or an unconfigured EVSE. SetNetworkProfile requires the station-only address.
Provisioning entries for an unconfigured address, another protocol, or a disabled
action fail startup even if their DTO shape is valid.

The private JSON format has required `variables` and `network_profiles` arrays;
root and entry objects reject unknown fields. The loader uses the existing canonical
absolute-path, effective-owner mode `0600`, regular-file, single-link and NOFOLLOW defenses,
checks opened inode identity and keeps provisioning outside the private state
directory and distinct from grants, station credentials and start identities.
Owner-only read buffers, decoded secret values and raw profiles wipe on error/drop;
stored secret types do not implement Debug or Serialize. Errors omit private content.
This is not a guarantee about third-party parser/transport buffers or OS copies.
See [the operator format and reference-only commands](../operations/headless-cli.md#protected-ocpp-201-device-and-network-configuration).

| Bound | Limit |
|---|---|
| Entire private startup file | 2 MiB (2,097,152 bytes), independently bounded before decode |
| Combined installed variables and network profiles | 128 |
| Aggregate provider secret content | 1 MiB (1,048,576 bytes) |
| One native profile JSON object | 64 KiB (65,536 bytes), depth at most 16 |
| One variable value | At most 1,000 Unicode characters; empty is valid |
| Component/variable names and instances | At most 50 Unicode characters each |
| Each resource identity string | At most 256 bytes |
| SetVariables local request/frame | At most 4,096 entries and 256 KiB complete encoded CALL |

### Fresh admission and actual-send fences

The daemon uses the **same provider Arc** for schema offering preflight and native
dispatch. For a fresh command, the existing registry validates its envelope and
the advertised schema offering checks every reference atomically against resource,
identity/slot, expiry and revocation **before durable admission**. There is no
public secret endpoint or separate authorization route. The existing exact-resource
and authenticated-origin retained-result retry hint runs before this fresh offering
check: a fully authorized identical historical retry can return its retained result
after expiry/rotation without resolving the old value or sending another CALL.
Changed content under the same request ID and foreign resource/origin cannot use
that historical hint as authority.

The actual socket owner re-resolves all references together and rechecks expiry,
revocation, generation and learned native limits immediately before send. Failure
sends no partial request. Counting includes the exact escaped complete native frame
without retaining an encoded secret frame in the queue; raw material is serialized
only at the send boundary. Provider, limit and generation fences linearize against
the first socket-send poll. Once asynchronous transmission begins, revocation cannot
unsend bytes and interruption cannot prove the station did not apply the change.
There are no probes, truncation, splitting, automatic retries or replay.

SetVariables shares the generation-owned LearnedLimits cache with independently
enabled GetVariables/GetBaseReport/GetReport sessions. Learn its exact
DeviceDataCtrlr ItemsPerMessage/BytesPerMessage identities with variable instance
`SetVariables` and attribute `Actual` through explicit GetVariables or validated
NotifyReport evidence. Unknown native limits permit only a conservative single-item
write; multi-item writes need both item and byte limits. Known limits and the local
bounds above apply at actual send, including changes while queued. Reconnect/restart
loses this proof; persisted query/report results cannot restore a new generation.

### Native outcomes, staging and durable recovery

SetVariables preserves each native `Accepted`, `Rejected`, `UnknownComponent`,
`UnknownVariable`, `NotSupportedAttributeType` and `RebootRequired`. Reordered
replies are valid only if every complete requested identity appears exactly once.
Missing, extra, duplicate or mismatched responses are transmission uncertainty,
not fabricated status or partial success. Aggregate acceptance is true only when
every item is Accepted or RebootRequired; mixed outcomes retain every item with
aggregate false. ReadOnly, wrong-format and out-of-range decisions remain genuine
native Rejected evidence; the bridge does not invent station metadata or rewrite
values.

SetNetworkProfile validates and retains every supplied pinned native field:
OCPP version/transport/interface, CSMS URL, signed native integers and full APN/VPN
configuration, including credentials and SIM PIN. Slot 0 is valid; slot and applicable
integer fields must fit signed 32-bit, while native policy determines valid settings.
The bridge never resolves or fetches the profile URL; describing SOAP or an older
OCPP version in a profile does not add bridge transport support. Preserve exact
Accepted/Rejected/Failed. **Accepted means stored/staged**, including replacement of
an active slot, not activation before a separately operator-controlled reboot.
The bridge issues no Reset, B10 migration, security-profile orchestration or
connectivity probe. Disconnect before an authoritative reply remains uncertain;
reconnect/restart proves neither activation nor permission to resend.

Optional action-tagged `CommandResult.configuration_201` contains identities or
configuration slot, native statuses and a network `staged` flag. It contains no
values, profiles, capabilities, statusInfo or customData. Native acknowledgement,
staging and later independently observed effects remain separate. Result v1.8,
nested export-record/export-batch schemas v1.9 and runtime export revision 8 retain
historical schemas/routes and existing target authorization/payload boundaries.
This addition uses existing atomic result JSON; SQLite stays at schema 14 with no
new SQL migration, configuration ledger or global export producer.

A fresh invalid reference fails preflight without a durable command (`GET` status
is 404). A command admitted but definitely not sent because of actual native budgets
returns HTTP 400 with a **flat Rejected CommandResult**; its retained `GET` is 200 and
matches the result. Neither rejection is a native acknowledgement. Ordinary native
response or uncertainty uses the existing HTTP 202 `{result}` envelope. On startup,
the coordinator pages all unresolved commands by request ID before listener binding,
changes crash-left Dispatched to TransmissionUncertain, preserves Admitted/already
uncertain state and advances past preserved rows without dispatching anything.
Historical retries return retained outcomes, never a resend.

The library's explicit `revoke(reference)` removes a capability under the provider
fence. The daemon exposes no revocation endpoint, watcher or hot reload. For operator
replacement/revocation, stop the daemon, safely replace its owner-only private file
with independently generated fresh capabilities for changed content/expiry, then
restart. Inspect retained uncertain results before any new intentional command.

The actual daemon's 13 retained configuration tests and an independent
software-peer smoke passed after integration. The smoke exercised native limits,
all statuses, case-folded/reordered identities, empty/max-Unicode values, contained
scope, durable-before-ACK and Heartbeat progress, staged versus separately rebooted
peer state, malformed uncertainty, expiry/rotation and disconnect/restart no replay.
Enabled redacted capture and public results excluded synthetic values/profiles/
capabilities; SQLite/WAL and logs excluded private values. These are software
boundary observations, not hardware interoperability, physical activation or OCA
certification. Broader monitoring, simulator completeness and production/provider
enablement remain separate.

## Verification and specification provenance

Requests and all native response statuses have independently authored wire fixtures validated
against unchanged OCA OCPP 2.0.1 Edition 4 schemas. The archive SHA-256 is
`192482c82a5e27a2319d2142be2d8c074b68a22851ff5a12d0541efc1eda775a`, as recorded in the existing
fixture provenance. Behavior follows Part 2 sections F01–F04 and B11–B12, including F01.FR.06,
F01.FR.13, F01.FR.19 and F01.FR.25 correlation, EVSE selection and scheduled-reset semantics.

`cargo test --locked -p uob-protocol-adapter --test ocpp201_remote_control` exercises real
authenticated WebSockets, scoped command admission, local authorization and SQLite. It verifies
native requests/statuses, persisted dispatch before reply, transaction commits independent of
acceptance, EVSE targeting, denial/expiry, sanitized evidence, late/malformed replies, reconnect,
process-loss recovery and no replay. The storage `remote_control` test verifies concurrent
allocation, restart, pruning, exhaustion and release-drain rejection. This is implementation
evidence, not OCA certification or physical charging verification.

`cargo test --locked -p uob-protocol-adapter --test ocpp201_trigger`,
`-p uob-storage-adapter --test trigger_observation_201`,
`-p uob-service --test remote_trigger_201` and
`-p uob-sim --test trigger_message_201 --test trigger_scenario_201`
exercise the opt-in trigger separately, including a running daemon with HTTP,
WebSocket and SQLite, and simulator ordering. These are implementation tests,
not OCA certification or a causal link from native acceptance to later reports.

## Protected native local authorization list and cache

GetLocalListVersion, SendLocalList and ClearCache use the existing durable
privileged demo command path, independently enabled only at the station root.
Admission dispatches by edition before applying native rules: identical action
names do not make a 201 request a 1.6 request. The owner-only
`local_authorization_updates_file` retains its existing updates entry shape,
while exact roster edition and capability prefix select separately typed providers.
Every update binds resource, edition, positive version, update type and expiry.
There is no public raw-list ingress, provisioning API, hot reload or installed-list
ledger; the service's exact-byte allowlist remains independent.

Public Send uses `urn:uob:ocpp201:SendLocalListReference:1` and camelCase
versionNumber/updateType/updateReference, with `list201:<64 hex>` capabilities.
Native protected metadata is validated against pinned Edition4 schemas and semantic
integer, identifierString, language, typed identity and expiry constraints. Generic
UTF-8 vendor/message fields remain valid, inert and private. Complete wire byte
counting includes escaping, actual message ID and CALL envelope, not just raw payload.
The bounded deferred send rechecks capability/grant authority, expiry, revocation,
current generation and smaller learned session limits before starting the socket send.
Queued Query/Clear also retain current action and generation authority.

Native positive-i32 versions, omission versus empty arrays and typed identity remain
201-specific; old 1.6 signed-version and Unicode rules are unchanged. Send native
Accepted/Failed/VersionMismatch and Clear Accepted/Rejected become value-free
`local_authorization_201` evidence. Query must report nonnegative i32.
Native statusInfo/customData do not become public diagnostics or result fields.
Malformed/lost replies and unsupported CALLERRORs use existing safe failure paths;
they do not manufacture native evidence, retry a mutation or turn Differential into Full.

Generation-local explicit correlated device-model results learn only exact
LocalAuthListCtrlr ItemsPerMessage/BytesPerMessage/Entries/Enabled/Available/
SupportsExpiryDateTime and AuthCacheCtrlr Enabled. Entries Actual (including zero)
is not capacity: numeric Integer maxLimit separately constrains Full count.
No instances or guessed DeviceDataCtrlr SendLocalList identity are accepted.
Old persisted device facts cannot restore authority on a new socket.

Result v1.10 and nested public export schemas v1.11 are additive; the runtime
export envelope is separately revision 10. Released files/routes and stored
record versions stay unchanged. SQLite remains schema14 with no migration.
HTTP/MQTT schema/result consumers gain neither privileged target ingress nor
automatic global export production. ACK, query, installed contents, offline use
and physical charging remain distinct evidence.

Retained regressions are executable assertions, not certification:

```text
cargo test --locked -p uob-contracts --test local_authorization201
cargo test --locked -p uob-application --test local_authorization201
cargo test --locked -p uob-storage-adapter --test local_authorization201
cargo test --locked -p uob-protocol-adapter local_authorization
cargo test --locked -p uob-service --test local_authorization201
cargo test --locked -p uob-mqtt-target-adapter --test local_authorization201_wire
```

The service suite starts the actual daemon and exercises authenticated admission,
native wire requests/results, durable SQLite/history, malformed/unpaired replies
and process restart non-replay. Native simulator behavior remains independently
implemented; joint service/simulator scenarios provide separate installed-state and
offline facts. These commands are verification instructions, not a claim they ran.


## Protected native reservations

ReserveNow and CancelReservation are default-off privileged demo actions. The public
ReserveNow wrapper carries only `id`, `expiryDateTime`, optional `evseId` and
`connectorType`, and a `reserve201:` capability. The owner-only `reservation201_file`
supplies the exact native request, including `idToken`/`groupIdToken`, which a deferred
socket encoder resolves only at the send boundary after rechecking the generation,
privileged grant, registration, capability, scope and expiry. An absent `evseId` needs
the explicit `reserve_non_evse_specific_supported` option (H01.FR.18/19). Every
reservation mutation is admitted with a durable `reservations201` revision in one SQLite
v16 transaction, single-flight per station.

Reconciliation uses only native facts. Accepted, Faulted, Occupied, Rejected and
Unavailable replies are kept exactly, and a same-ID replacement supersedes the previous
owner only on Accepted (H01.FR.02). `ReservationStatusUpdate` Expired/Removed is
committed before its empty acknowledgement (H01.FR.16/17, H04.FR.01). A
`TransactionEvent` `reservationId` consumes a reservation only on its EVSE and, when an
idToken is present, only for the reserved type-scoped, case-folded identity or a
provisioned group member (H01.FR.15, H03). Reused IDs follow the 1.6 chronology rules.
Trusted expiry also runs offline. The bridge never infers removal from a
StatusNotification, and a transaction report never grants authorization.

```text
cargo test --locked -p uob-contracts --test reservation201
cargo test --locked -p uob-application --test reservation201
cargo test --locked -p uob-storage-adapter --test reservation201
cargo test --locked -p uob-protocol-adapter --test ocpp201_reservations
cargo test --locked -p uob-service --test reservations201
python3 bins/uob-sim/tests/reservation201_joint_smoke.py --bridge target/debug/uob \
  --simulator target/debug/uob-sim --output <fresh private directory>
```

The joint smoke runs the actual daemon and the independent simulator as separate
processes through a recording loopback relay. It is opt-in and not part of the workspace
verifier. These commands are verification instructions, not a claim they ran.

## Opt-in composite schedules and installed-profile reports

`get_composite_schedule = true` (now also accepted for OCPP 2.0.1 stations) and
`get_charging_profiles = true` (OCPP 2.0.1 only) are independently default-off
privileged demo actions. Both require the control and privileged grant files, reject
configured native EVSE or connector IDs above `i32::MAX`, and are advertised for the
station and each configured positive EVSE, never for a connector.

GetCompositeSchedule (K08) uses schema `urn:OCPP:Cp:2:2020:3:GetCompositeScheduleRequest`.
Station scope must send `evseId: 0`, the grid connection (K08.FR.03); EVSE scope must send
its exact native ID. `duration` is 1–2147483647 seconds and the optional unit is exactly
`A` or `W`. `customData`, unknown fields, nulls and out-of-scope IDs fail before a CALL.
Accepted requires a schedule for the requested EVSE, within the requested duration and in
the forced unit when one was requested (K08.FR.02/07), with periods that start at zero,
strictly increase and stay inside the horizon. Limits are exact tenths including zero;
`numberPhases` is 1–3 and `phaseToUse` only accompanies a single phase. A Rejected reply
(K08.FR.05) maps to `protocol_rejected` with typed evidence and may carry a validated
schedule. Anything else is `transmission_uncertain` without evidence; a CALLERROR is a
sanitized rejection. The schedule is the charger's indicative calculation: the bridge
neither calculates nor enforces it and records no snapshot change or physical effect.

GetChargingProfiles (K09) uses schema `urn:OCPP:Cp:2:2020:3:GetChargingProfilesRequest`.
Station scope may omit `evseId` (every EVSE), send zero (only the grid connection) or name
one EVSE; EVSE scope must name itself. The criterion carries either profile IDs (at most
64) or at least one of `stackLevel`, `chargingLimitSource` and `chargingProfilePurpose`,
never both (K09.FR.03). Admission registers the signed `requestId` in the connection's
`ReportChargingProfiles` namespace before the CALL is queued; a reused ID is refused.

One task owns each query. It records the native acknowledgement durably before releasing
the command outcome: `Accepted` leaves the report `pending`, `NoProfiles` makes it
`not_expected` and maps to a `protocol_response` with `accepted: false` and no error.
Fragments are routed by the socket owner, answered with an empty
`ReportChargingProfilesResponse` and sanitized against the exact query. A fragment for
another EVSE, an unrequested limit source or a profile that does not match every supplied
criterion is a correlation failure (K09.FR.04-06); a schema-invalid fragment gets a
CALLERROR. Fragments are retained in arrival order until one without `tbc`
(K09.FR.01/02), within the shared multipart bounds and the 30-second deadline from actual
dispatch. Each reported profile keeps its EVSE, `chargingLimitSource`, purpose, kind,
validity and one to three schedules with exact limits; `customData` is dropped and a
`salesTariff` is omitted and flagged. Fragments that arrive before the reply are held in
memory and recorded with it, never before.

Reports end `incomplete` on a correlation failure, an invalid fragment, a limit, the
deadline or a disconnect, with the accepted progress and no partial inventory. A restart
terminalizes a pending report as `interrupted` through the existing startup report
reconciliation. Terminal reports are frozen against later writers, and nothing is
replayed: duplicates return the durable result and a reconnect sends no CALL. Late
fragments for a finished request are acknowledged on the wire but never retained.
Reported profiles describe charger state at report time; they never become local policy,
ownership metadata or evidence of physical charging.

```text
cargo test --locked -p uob-contracts --test schedules201
cargo test --locked -p uob-storage-adapter --test schedules201
cargo test --locked -p uob-protocol-adapter --test ocpp201_schedules --test command_registry
cargo test --locked -p uob-service --test schedules201
```

The service suite starts the actual daemon with an independent WebSocket peer and
exercises authenticated admission, exact HTTP evidence, report completion, heartbeat
progress during a delayed reply, a crash after acknowledgement and restart without replay.
These commands are verification instructions, not a claim they ran.
