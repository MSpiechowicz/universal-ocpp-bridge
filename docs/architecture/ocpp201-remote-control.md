# OCPP 2.0.1 remote control

`v201::remote_control::RemoteControlSession` implements the existing application
`StationCommandPort` for one authenticated OCPP 2.0.1 socket. Compose it with the ordinary durable
`CommandCoordinator` and scoped access guard. Start and stop require control permission; native
Reset and UnlockConnector require privileged control. There is no additional HTTP endpoint or
authorization path. As with the 1.6 implementation, the management-only executable is not yet a
composed charging host.

The owner supplies committed registration, topology, capabilities, availability and transaction
state. Updates cannot change station identity or connection epoch, or move observation time
backward. Reconnect requires a new port. Old commands are never queued for a replacement socket.

| Operation | Canonical request | Native behavior |
|---|---|---|
| RequestStartTransaction | Start with a locally authorized reference; station scope or explicit EVSE scope | Optional evseId, a durable remoteStartId, typed idToken; Accepted/Rejected and optional native transactionId |
| RequestStopTransaction | Stop with a retained canonical transaction ID in the addressed station, EVSE or connector | Native transactionId from committed protocol evidence; Accepted/Rejected |
| Reset | Privileged Ocpp with `urn:OCPP:Cp:2:2020:3:ResetRequest`, Immediate or OnIdle | Station scope omits evseId; EVSE scope must match it exactly; Accepted/Rejected/Scheduled |
| UnlockConnector | Privileged Ocpp with `urn:OCPP:Cp:2:2020:3:UnlockConnectorRequest` | Both positive EVSE and connector IDs must match the addressed connector; Unlocked/UnlockFailed/OngoingAuthorizedTransaction/UnknownConnector |

A start cannot select a connector on the wire. Connector-scoped starts fail explicitly rather
than widening permission to their EVSE. Station-scoped starts omit evseId and require station-scoped
local authorization. The service conservatively refuses starting on an EVSE with a retained live
transaction; it does not infer from Pending alone that an existing transaction is unauthorised.
Unavailable resources, missing capabilities, unknown payload fields/schemas, unsupported native
operations and violated advertised parameters fail closed. Optional charging profiles and group
tokens require their separately planned feature support; raw privileged starts cannot bypass the
normal start guard.

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
