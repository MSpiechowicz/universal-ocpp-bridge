# OCPP 1.6 availability

`v16::remote_control::RemoteControlSession` dispatches ChangeAvailability through the existing
scoped, durable command coordinator. The request is a privileged OCPP operation with schema
`urn:OCPP:1.6:2019:12:ChangeAvailabilityRequest` and exactly `connectorId` and `type` fields.
Only advertised operations on registered resources can dispatch. The wire connector must match
the authorized resource: zero requires station scope and means the controller and all connectors.
Unknown fields, native IDs, schemas, types, expired requests and insufficient permissions fail
explicitly. No additional command endpoint or authorization path is introduced.

The session takes an application-owned `RemoteControlStore` backed by the same operational
SQLite worker. It persists native Accepted, Scheduled or Rejected response evidence before
returning the protocol result. Scheduled is accepted for later execution; it does not change
observed availability, stop a transaction or imply that charging has stopped. A failure to retain
native evidence is an uncertain result. Evidence uses the existing per-command retention row;
no schema migration or independent pending queue is needed.

## Observations and recovery

The ordered station owner uses `v16::availability::complete_status` for StatusNotification when
availability journal evidence is required. This replaces the ordinary status handler for that
call. The application applies the existing registration/status rules, then commits the snapshot
and a `station.availability.observed` journal event atomically before replying. The host supplies
trusted runtime identity, a unique event ID and the next station event sequence. It publishes
the committed snapshot to the session only after success. Event payloads are typed station
snapshots capped at 256 KiB; a host business-event enum can wrap them through `From<StationSnapshot>`.
Journal retention and storage pressure remain governed by the existing operational store.

Read that event from the authoritative journal, pass its typed snapshot to
`v16::availability::observed_effect`, and link any returned effect through the ordinary command
coordinator. The result preserves the original protocol response separately. A matching observation
is compatible evidence, not proof of unique causation: OCPP 1.6 supplies no remote-request ID in
StatusNotification. No observation submits another command.

Connector requests require fresh good-quality status for that exact connector. Station requests
require it for the controller and every known connector. Inoperative requires Unavailable and
no retained non-ended transaction in the addressed scope. Operative accepts Available and the
native occupied/charging states; Faulted and Unavailable never demonstrate operative availability.
Missing timestamps use bridge observation time. Old source timestamps cannot confirm a new
request, and stale notifications cannot rewind persisted state. Transaction completion continues
to belong to the transaction workflow, independently of status or command responses.

A controller Unavailable/Faulted observation also prevents remote starts even if a connector's
last reported status was Available. The controller never becomes a synthetic charging connector.
A rejected request leaves state untouched. An already-matching request still awaits the charger's
response and a fresh observation; it does not manufacture success locally.

Restart retains requested parameters, native Scheduled evidence and observed connector state.
Reconnect creates a new session port from recovered state. Duplicate requests return the retained
result without sending; dispatched requests lacking an authoritative outcome remain uncertain.
Delayed, malformed and late responses cannot invent physical effects or trigger automatic replay.
As with the other protocol workflows, the management-only executable is not yet a composed
charging host; these ports are exercised over real authenticated sockets in integration tests.

## Specification and verification

Behavior follows the pinned OCA OCPP 1.6 Edition 2 sections 5.2, 6.7–6.8, 7.3–7.4 and the existing
StatusNotification rules. The unchanged ChangeAvailability schemas were extracted from the
archive already recorded in `tests/ocpp-fixtures/corpus/provenance.json`, with verified SHA-256
`2a1d80284ca60449e85951fc55bc0538b5d45bd6f97c6d9228745acacb52c11b`. Seven hand-authored wire
fixtures and their schema digests are registered in the independent corpus. This is behavioral
evidence, not OCA certification.

The `ocpp16_availability` integration suite covers connector/station and both requested types,
all native response statuses, a scheduled active transaction, live status handling during a delayed
response, scope/permission/parameter failures, atomic persistence failure, old evidence,
restart/reconnect, late/malformed responses and process loss without replay. It asserts SQLite
snapshots, native evidence, command results and actual peer frames. The existing transaction and
registration suites separately exercise transaction completion and status-state persistence.
