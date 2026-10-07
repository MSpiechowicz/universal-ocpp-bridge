# OCPP 1.6 remote control

`v16::remote_control::RemoteControlSession` implements the application `StationCommandPort` for
one authenticated OCPP 1.6 socket. Compose the existing `CommandCoordinator` and scoped access
guard around it: ordinary `Start`/`Stop` require control permission; pinned `Reset`,
`UnlockConnector`, configuration, `TriggerMessage`, `GetCompositeSchedule`,
`SetChargingProfile` and `ClearChargingProfile` operations require privileged control.
No new HTTP or authentication path is introduced. The service composes a charging host for
explicitly opted-in, loopback-only demo ingress; production charging commands remain disabled.

The station owner supplies its latest **committed** snapshot, including accepted registration,
explicit operation capabilities, availability, and native transaction evidence. It publishes
updates with `update_committed`; the port rejects identity changes, older observations, and a new
connection epoch. Construct a new port on reconnect and route commands through the live station
registry. Old handles cannot send through a replacement socket. A disconnected command fails
before durable admission, and a disconnect racing dispatch never queues work for reconnect.

| Operation | Canonical input and validation | Native response |
|---|---|---|
| RemoteStartTransaction | `Start` with a local authorization reference; an available connector without a live transaction; station scope allows charger selection | Accepted / Rejected |
| RemoteStopTransaction | `Stop` with a canonical transaction ID belonging to the addressed resource; wire ID comes from persisted OCPP 1.6 evidence | Accepted / Rejected |
| Reset | Privileged `Ocpp`, action `Reset`, schema `urn:OCPP:1.6:2019:12:ResetRequest`, payload `{"type":"Soft"}` or `{"type":"Hard"}`; station scope only | Accepted / Rejected |
| UnlockConnector | Privileged `Ocpp`, action `UnlockConnector`, schema `urn:OCPP:1.6:2019:12:UnlockConnectorRequest`, payload connector ID matching an existing positive connector | Unlocked / UnlockFailed / NotSupported |
| GetConfiguration | Privileged `Ocpp`, action `GetConfiguration`, pinned OCA request schema; station scope, absent/empty/selected key lists and learned `GetConfigurationMaxKeys` bound | Known keys (including read-only flags), unknown keys and optional omitted lists |
| ChangeConfiguration | Privileged `Ocpp`, action `ChangeConfiguration`, bridge reference schema `urn:uob:ocpp16:ChangeConfigurationReference:1`; station scope and locally provisioned key-bound, expiring reference | Accepted / Rejected / RebootRequired / NotSupported |
| TriggerMessage | Privileged `Ocpp`, action `TriggerMessage`, schema `urn:OCPP:1.6:2019:12:TriggerMessageRequest`; six pinned native classes with station/connector scope | Accepted / Rejected / NotImplemented |
| GetCompositeSchedule | Privileged `Ocpp`, action `GetCompositeSchedule`, schema `urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest`; station connector 0 or exact positive connector, positive i32 duration and optional A/W unit | Accepted with meaningful typed schedule / Rejected |
| SetChargingProfile | Privileged `Ocpp`, pinned `urn:OCPP:1.6:2019:12:SetChargingProfileRequest`; complete native profile, exact station/connector scope and ongoing transaction checks for TxProfile | Accepted / Rejected / NotSupported |
| ClearChargingProfile | Privileged `Ocpp`, pinned `urn:OCPP:1.6:2019:12:ClearChargingProfileRequest`; station authority for ID/broad clears, exact connector for child filter clears | Accepted / Unknown |
| UpdateFirmware | Privileged `Ocpp`, bridge reference schema `urn:uob:ocpp16:UpdateFirmwareReference:1`; station scope, provider artifact of kind `Firmware`, durable job before dispatch (see [OCPP 1.6 firmware](ocpp16-firmware.md)) | Empty acknowledgement / CALLERROR |
| GetDiagnostics | Privileged `Ocpp`, bridge reference schema `urn:uob:ocpp16:GetDiagnosticsReference:1`; station scope, provider upload destination bound to a durable job before dispatch (see [OCPP 1.6 diagnostics](ocpp16-diagnostics.md)) | fileName / no fileName / CALLERROR |
| GetLog | Privileged `Ocpp`, bridge reference schema `urn:uob:ocpp16:GetLogReference:1`; station scope, `DiagnosticsLog` or `SecurityLog` destination bound to a durable job | Accepted / Rejected / AcceptedCanceled |
| SignedUpdateFirmware | Privileged `Ocpp`, bridge reference schema `urn:uob:ocpp16:SignedUpdateFirmwareReference:1`; station scope, `SignedFirmware` artifact whose signing certificate the PKI provider trusts | Accepted / Rejected / AcceptedCanceled / InvalidCertificate / RevokedCertificate |

Unknown fields, wrong schemas, OCPP 2.0.1 reset types, unsupported operations and cross-resource
native addresses fail closed. Unlock connector IDs use real topology rather than the pinned
model library's non-specification maximum of 20. Optional start charging profiles belong to the
separate smart-charging feature and are not accepted through a raw privileged start bypass.

`LocalRemoteStartIdentity` owns at most 128 locally configured idTags of 1–20 characters. It
computes the same SHA-256 references as the existing local authorization provider and rechecks the
persisted allowlist, resource scope, expiry and revocation at dispatch. Commands store only the
opaque reference; token material is resolved only to construct the bounded socket payload.
A custom `RemoteStartIdentity` must provide equivalent bounded local policy checks. Charger-side
Authorize/StartTransaction requests still use normal local authorization independently of the
charger's `AuthorizeRemoteTxRequests` setting. Stop does not require renewed start permission.

Configuration writes persist only `key` and an opaque `valueReference`; the actual value is held
by `LocalConfigurationValues`. The queue contains only that reference; the authenticated
socket owner rechecks station, key, expiry and revocation immediately before encoding and sending
the native CALL. There is no inline-value fallback. Revocation after the asynchronous send starts
cannot cancel that in-flight transmission. Construct and provision the provider in the trusted
host; the demo charging host opts in to its supported command actions independently, without a
new configuration endpoint. Once a read has shown a key is read-only, a write to that key is
denied locally for that socket session.

The opt-in management command route requires a per-request bearer credential verified by the
host's `ManagementCommandAuthenticator`; it never accepts an origin supplied by the request body.
Command status and history use the separate authenticated, station-scoped event read grant;
command submission credentials do not authorize these reads. An integrator must install real
verifiers for both credentials before mounting the combined command-and-read route; a host-owned
fixed principal alone is not sufficient.

Expiry is checked at admission, preparation, and immediately before queueing, then translated to
a monotonic last-send deadline checked by the socket owner. Extremely distant expiries are capped
to 24 hours of queue residence. Calls use the existing shared message/pending-request budgets and
response timeout. Timeout, invalid response payload, lost socket, or lost session task remain
`transmission_uncertain`. Valid native denials preserve their status; CALLERROR is a protocol
rejection with remote free text excluded. Late replies cannot rewrite a durable result.

`TriggerMessage` is admitted only for a live OCPP 1.6J station that advertises the
action and has the privileged grant. The permitted `requestedMessage` values are
`BootNotification`, `DiagnosticsStatusNotification`, `FirmwareStatusNotification`,
`Heartbeat`, `MeterValues` and `StatusNotification`. A Boot trigger is allowed only
before registration is Accepted (including Pending); after Accepted the station must
initiate a new boot before another Boot trigger is permitted. For the four station-only
classes, `connectorId` is irrelevant. An explicit `connectorId: 0` addresses only
station `StatusNotification`, never `MeterValues`. Positive IDs must match the addressed
connector for status or metering. Omitted ID requests all applicable configured targets:
station 0 plus connectors for status, positive connectors only for metering. The native
target set is frozen before dispatch, bounded to 64 connectors; a topology change does
not retroactively broaden that expectation. Invalid/unsupported scope does not reach
the wire.

## Responses and observed state

The coordinator persists the command before dispatch and persists the correlated outcome before
returning it. A successful response does not change a transaction or fabricate a physical effect.
Use ordinary registration/status and transaction handlers for later charger observations; their
commits remain authoritative. Reset does not discard active transactions. Unlock is not a stop
shortcut; the charger must still report any resulting StopTransaction normally.

The bounded journal consumer can use `observation::transaction_effect` to match a committed event
to a retained command by resource, timestamps, authorization reference (start), or canonical
transaction ID (stop). Pass the resulting effect through the existing coordinator reconciliation
method after the command's dispatch future completes. Re-reading the durable event after restart
is safe: duplicate event IDs are idempotent. OCPP 1.6 does not echo a remote request ID in these
notifications, so this is compatible observed evidence, not proof of unique causation. A reported
start remains `pending`, not proof of power flow. A stop, status change, or reconnect alone cannot
prove physical reset/unlock success and is never labeled as such.

For `TriggerMessage`, the exact native `Accepted`, `Rejected` or `NotImplemented`
reply is retained independently of later station calls. A dispatch-started 60-second
window can link only committed, compatible class/station/target events to the fixed
expectation. `pending` means a compatible set is not yet complete within the window;
`partial` means some targets, but not all, were observed; `observed` means every expected
target has compatible evidence after native `Accepted`; `absent` means the deadline
elapsed without a compatible event; `unsupported` records native `Rejected` or
`NotImplemented`. A missing response is never treated as Accepted. These are observation
states, not proof that the trigger caused a call or that charging occurred: OCPP 1.6
station calls carry no trigger request ID. A reply must precede the station's requested
CALL on the wire; late/malformed replies or lost sockets remain uncertain, and neither
restart nor reconnect automatically replays the command. A pending observation is
reconciled from durable events across restart without retransmission.

Uncertain and interrupted dispatches are recovered without retransmission. An identical retry
returns the stored result; an explicit new operator request is required for another attempt.
No service restart or station reconnect silently executes old commands.

GetConfiguration results retain the requested keys, returned keys, unknown keys, read-only
flags and whether a value was absent or redacted. Only a small allowlist of numeric OCPP Core
settings discloses a validated scalar; unknown/vendor and credential-like settings never expose
their values in durable results, command status, exports, MQTT or diagnostic capture. A
ChangeConfiguration acknowledgement records the exact native status, not proof that a setting
changed. A later explicit GetConfiguration may be linked with
`CommandCoordinator::reconcile_configuration_observation`; the read and write remain separate
requests and no reboot or retry is initiated automatically.

## Opt-in GetCompositeSchedule

`get_composite_schedule = true` is a default-off, OCPP 1.6J-only station option. The demo host
requires separate control and privileged grant files; the submitting request must use the
privileged credential. Demo-only, loopback-only ingress restrictions are unchanged. Opt-in
configuration rejects any configured native connector ID above `i32::MAX` (2147483647);
leaving the option off preserves legacy topology behavior.

Admission and dispatch require Accepted registration, the advertised action for the exact
resource, a live authenticated socket and the existing scoped authorization/expiry checks.
Station scope requires `connectorId: 0`, meaning the charger-calculated grid aggregate, not
an arbitrary connector. Connector scope requires its exact configured positive native ID.
The pinned request requires integer `connectorId` and `duration`; duration is 1–2147483647
seconds. Optional `chargingRateUnit` is exactly `A` or `W`; omission remains omission.
Wrong edition/schema/resource, unknown fields, present nulls, missing required fields,
fractional/overflow integers and invalid units fail before a native CALL.

The optional `CommandResult.composite_schedule_16` retains immutable request context and exact
native `Accepted`/`Rejected` status. It preserves supplied connector identity, `scheduleStart`,
schedule duration, independent `startSchedule`, unit, periods, `numberPhases` and
`minChargingRate` as typed snake_case fields; omitted metadata stays absent. Timestamp
instants normalize to UTC without inventing a start time. Native numeric rates become exact
canonical decimal **strings**, including `900719925474099.1`; no binary floating-point
rounding or implicit A/W conversion occurs.

Accepted requires `scheduleStart` and a complete, meaningful schedule with nonempty periods,
first `startPeriod = 0`, strictly increasing nonnegative starts within the applicable horizon,
and a valid A/W unit matching any forced request unit. Supplied duration is positive and
cannot exceed the request; an omitted duration uses the request horizon only for validation,
not as fabricated response metadata. Supplied connector identity must match the request.
Limits and optional minimum rate must be nonnegative and have at most one meaningful
fractional decimal digit; trailing zeros and exponent notation normalize only when exact.
Supplied phases must be positive native integers; omission remains absent, with no invented
upper bound of three. A genuine zero/off period is retained even when `minChargingRate` is
positive. A Rejected reply may omit the schedule, but any supplied metadata must still be valid.

Valid native Rejected maps to `protocol_rejected` with typed Rejected evidence, not an empty
Accepted schedule. Malformed or semantically invalid CALLRESULTs, including null/unknown
nested fields, map to `transmission_uncertain` without fabricated schedule evidence.
A valid CALLERROR uses existing sanitized protocol rejection and does not manufacture a
Rejected CALLRESULT. Timeout/disconnect and interrupted dispatch remain uncertain.

The query is indicative charger evidence only. It neither alters station snapshots nor
creates physical effects, computes a local schedule, installs/removes profiles or enforces
charging. Results persist across restart; exact duplicates return the original result without
another CALL. Restart/reconnect never automatically replays a query. A new explicit request
is required for another query after reconnect, and replies from an old socket generation
cannot attach to it or resolve a terminal uncertain result. No simulator smart-charging engine
or OCA certification is established by this feature. The separate OCPP 2.0.1 query is described
in [OCPP 2.0.1 remote control](ocpp201-remote-control.md#opt-in-composite-schedules-and-installed-profile-reports).

## Opt-in native charging profiles

`set_charging_profile` and `clear_charging_profile` are independent, default-off OCPP 1.6J
station options. Either requires distinct control and privileged grant files; submission uses
the privileged grant, not the ordinary control grant. Enabling one does not enable the other
or canonical `SetChargingLimit`. Demo-only, loopback-only ingress and Accepted registration,
advertised per-resource capability, exact live resource/socket generation, authorization and
expiry checks still apply at admission and dispatch. Opt-in rejects configured native connector
IDs above `i32::MAX`; native positive connector IDs must resolve to configured children.

The full native Set is separate from canonical `SetChargingLimit`, even though the latter
also uses a bounded native TxProfile internally. Canonical unit conversion, positive limits
and phase policy remain unchanged; canonical results do not acquire `charging_profile_16`.
The full privileged request preserves native purpose, signed i32 profile/transaction IDs,
nonnegative i32 stack, `Absolute`/`Recurring`/`Relative` kind, and optional `Daily`/`Weekly`
recurrence. `recurrencyKind` is permitted only with `Recurring` but is not made mandatory.
Supplied `validFrom`, `validTo` and `startSchedule` remain supplied timestamp evidence
(typed instants normalize to UTC); omitted anchors/validity remain absent. Native validity
windows, including expired windows, remain charger decisions rather than bridge clock policy.

`ChargePointMaxProfile` requires station scope and connector 0. `TxDefaultProfile` permits
station 0 or the exact positive addressed connector. `TxProfile` requires that exact positive
connector and a supplied `transactionId` matching one unique established ongoing native
transaction at dispatch. Pending, Active or Suspended is eligible only with native transaction
evidence and no stop; missing, ended, uncertain or ambiguous identity fails before wire.
Other purposes prohibit `transactionId`.

Native `A`/`W` rates and optional `minChargingRate` are exact nonnegative numeric quantities
with at most one meaningful fractional decimal digit. Zero is legal and retained, including
with a positive minimum rate. Exact trailing zeros/exponent notation may normalize for decoding;
overflow, precision loss and extra meaningful fractional digits fail, never round or clamp.
No voltage/phase conversion or invented charger capacity is applied. Optional `numberPhases`
is a positive native i32, not the canonical limit's 1–3 policy. There are 1–1024 periods:
first `startPeriod` is 0 and subsequent nonnegative i32 starts strictly increase. Supplied
duration is a nonnegative i32, including native zero. Periods beyond duration or recurrence
are retained for the charger's truncation semantics, not rejected using composite-reply rules.
The fully encoded Set/Clear native CALL has a profile-only 256 KiB ceiling; tighter existing
message, queue and pending-request budgets still apply. Installed count, supported units,
maximum stack and hardware capacity are not discovered or guessed.

For Clear, supplied signed i32 `id` overrides all other selectors on the charger, even a
supplied connector filter. Therefore every ID clear requires station authority. Empty,
omitted/wildcard connector or connector-0/broad clears also require station authority.
A connector-scoped filter clear must omit `id` and supply that child's exact positive native
`connectorId`; supplied purpose and nonnegative stack filters combine with AND semantics.
Selectors remain captured even when ID makes them irrelevant. Accepted/Unknown status does
not identify which profiles were removed; the bridge does not invent an installed-profile inventory.

Optional action-tagged `CommandResult.charging_profile_16` freezes the full Set request or
Clear selectors and the valid native CALLRESULT. Set retains `Accepted`, `Rejected` or
`NotSupported`; Clear retains `Accepted` or `Unknown`. Native denial maps to
`protocol_rejected` with typed status. A valid CALLERROR is sanitized protocol rejection without
an invented CALLRESULT. Malformed replies, timeout, disconnect and interrupted dispatch remain
`transmission_uncertain` without fabricated typed reply evidence. Terminal evidence is immutable
against late replies/conflicting writes, survives SQLite reopen/restart, and exact duplicates
return the original result. Neither reconnect nor restart replays an uncertain command.

The browser accepts protected option discovery up to 512 descriptors, 24 fields per
descriptor and 128 characters per field name, within the unchanged 1 MiB response cap.
The current maximum topology (64 children plus the station, up to five native action
families) needs at most 325 descriptors; discovery is not truncated. Complex native
Set has no supported browser editor: use the authenticated programmatic management API described
in [headless operations](../operations/headless-cli.md). This is not a raw JSON browser bypass.
Native acceptance proves neither physical enforcement nor a later charging effect.

## Verification and provenance

The independent fixture corpus includes all four requests (both reset modes) and every native
response status. Its schemas are unchanged bytes from the already pinned OCA OCPP 1.6 Edition 2
archive, SHA-256 `2a1d80284ca60449e85951fc55bc0538b5d45bd6f97c6d9228745acacb52c11b`.
Behavior follows sections 5.11, 5.12, 5.14 and 5.18, plus the published errata bundled with that
archive. Fixtures are hand-authored; model serialization does not generate expected wire data.

`cargo test --locked -p uob-protocol-adapter --test ocpp16_remote_control` uses authenticated real
WebSockets, the hostile peer, scoped application admission, local authorization and real SQLite.
It checks persisted dispatch before reply, independent transaction commits/effects, denial and
invalid input, expired queued work, late and malformed replies, disconnection, process-loss
recovery and no automatic replay. The existing transaction/registration suites verify status
ordering, reconnect continuity and failed commits. This is implementation evidence, not OCA
certification or proof of physical charging hardware behavior.

`cargo test --locked -p uob-protocol-adapter --test ocpp16_configuration --test
ocpp16_configuration_outcomes --test ocpp16_configuration_policy` exercises the real
authenticated WebSocket and SQLite command path: partial/unknown and redacted reads,
read-only/unauthorized/disconnected writes, exact native statuses, request deduplication,
later explicit observations and restart recovery. Pinned OCA GetConfiguration and
ChangeConfiguration wire schemas and independent JSON examples are checked by
`cargo run --locked -p uob-ocpp-fixtures`. This does not establish interoperability with
physical stations or OCA certification.

`cargo test --locked -p uob-protocol-adapter --test ocpp16_remote_control trigger`
exercises real authenticated socket dispatch, all six classes, native statuses,
scope and registration rejection, bounded all-connector targeting, delayed reply
and disconnect uncertainty. `cargo test --locked -p uob-storage-adapter --test
trigger_observation` covers durable compatible-event matching, deadline states
and restart; `cargo test --locked -p uob-service --test remote_trigger -- --nocapture`
covers opt-in grants and the running service's HTTP/WebSocket/SQLite observation
and 60-second restart path; `cargo test --locked -p uob-sim --test
trigger_message` covers scoped station replies, delayed/omitted observations
and reconnect. The independent trigger wire fixtures are checked by
`cargo run --locked --package uob-ocpp-fixtures`. None proves native
cause-and-effect, physical charging or OCA certification.

`cargo test --locked -p uob-protocol-adapter --test ocpp16_composite_schedule --test
command_registry` exercises independent authenticated socket replies, exact decimal boundaries,
scope/authority denial before wire, malformed-response uncertainty, zero versus Rejected,
delayed replies with heartbeat progress and one-shot recovery. `cargo test --locked -p
uob-storage-adapter --test composite_schedule` checks terminal evidence against stale/conflicting
writers, close/reopen and older result JSON.

`cargo test --locked -p uob-service --test composite_schedule` passed the four actual-process
scenarios `native::normalizes_and_persists_native_schedule`,
`admission::denies_invalid_scope_and_authority_before_wire`,
`recovery::delayed_reply_keeps_heartbeat_progress` and
`recovery::disconnect_restart_never_replays_schedule_query`. A separate actual-daemon smoke
also observed exact HTTP/SQLite evidence, authenticated current/historical EMS schemas,
heartbeat progress, zero versus Rejected, identical results after restart, quiet reconnect
and a new explicit query. These are software implementation evidence, not hardware validation
or a full smart-charging/certification claim.

`cargo test --locked -p uob-mqtt-target-adapter --test ingress_wire --test outbound_wire`
verifies immediate and durable result publication on existing topics for command-result
v1.0/v1.1 and additive v1.4 composite schedule evidence. The existing v1.2/v1.3 policy is
unchanged; this is not support for every minor revision or a new MQTT command family.
Broker PUBACK acknowledges broker receipt, not native acceptance or physical success.

For native profiles, `cargo test --locked -p uob-protocol-adapter --test
ocpp16_charging_profiles` passed 11 real-socket regressions covering exact rates, native
statuses, signed/period/frame boundaries, transaction and selector authority, immutable
request/result evidence and no-replay uncertainty. The canonical `charging_limit` and
`ocpp16_composite_schedule` suites also passed; their distinct semantics are unchanged.
`cargo test --locked -p uob-storage-adapter --test charging_profile` passed both terminal
conflict/reopen and late-reply uncertainty regressions.

`cargo test --locked -p uob-service --test charging_profiles` passed eight actual-daemon
scenarios, including independent opt-ins/grants, native A/W/zero/statuses, denied admission,
malformed reply/CALLERROR/30-second timeout, heartbeat progress and apply-disconnect-restart.
A separate rebuilt-daemon smoke with an independently authored RFC 6455 peer observed
A 8.1, A 0 and W 7200.1, all action-specific statuses, revision-6 typed results, denied
connector ID-clear/invalid tenths/ordinary-control submissions without a CALL, heartbeat
before the profile reply, and retained uncertainty without resend after reconnect/restart.
The software peer reloaded its own profile state; eight terminal results remained readable.
This is bounded software-peer evidence, not hardware enforcement or a full simulator.

The narrow `ocpp16.smart-charging.profiles` corpus row links executable regressions and eleven
hand-authored wire fixtures. Its four attributed OCA schemas retain original bytes; details
are in [fixture provenance](../testing/ocpp-fixture-corpus.md). Current command-result v1.6
and nested export v1.7 schemas preserve historical snapshots/routes. HTTP/MQTT tests cover
their existing result-reader/publisher boundaries; native methods do not create an automatic
external-export producer. SQLite remains v13 with no profile SQL migration.

`UOB_LIVE_BROWSER=1 npm --prefix frontend run test:browser -- live-command.browser.ts`
passed one real-daemon scenario with both-edition peers and native profile actions advertised.
A direct browser inspection also loaded protected options containing Set/Clear and long
nested field paths while existing controls remained available. This proves metadata/control
compatibility, not a nested profile editor. Whole-workspace verification and fresh reviews
remain separate gates. Broad smart charging/enforcement and OCA certification
remain outside this narrow implementation.
