# OCPP 1.6 remote control

`v16::remote_control::RemoteControlSession` implements the application `StationCommandPort` for
one authenticated OCPP 1.6 socket. Compose the existing `CommandCoordinator` and scoped access
guard around it: ordinary `Start`/`Stop` require control permission; pinned `Reset` and
`UnlockConnector` operations require privileged control. No new HTTP or authentication path is
introduced. The management-only service executable is not yet a composed charging host.

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

Expiry is checked at admission, preparation, and immediately before queueing, then translated to
a monotonic last-send deadline checked by the socket owner. Extremely distant expiries are capped
to 24 hours of queue residence. Calls use the existing shared message/pending-request budgets and
response timeout. Timeout, invalid response payload, lost socket, or lost session task remain
`transmission_uncertain`. Valid native denials preserve their status; CALLERROR is a protocol
rejection with remote free text excluded. Late replies cannot rewrite a durable result.

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

Uncertain and interrupted dispatches are recovered without retransmission. An identical retry
returns the stored result; an explicit new operator request is required for another attempt.
No service restart or station reconnect silently executes old commands.

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
