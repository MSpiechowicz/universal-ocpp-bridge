# Correlated diagnostic instrumentation

The application owns `FlowDiagnostics`, a cloneable, optional process emitter, and
`FlowSpan`, an owned context that can cross queues and awaits. Construct one emitter
with the runtime process/bridge identities, the existing `CaptureManager`, a trusted
UTC clock, and a bounded receiver. Pass clones through `Application::with_diagnostics`,
`CommandCoordinator::with_diagnostics`, `ScopedCommandAdmissionPort::with_diagnostics`,
and `spawn_target_session_with_diagnostics`. Default constructors leave instrumentation
disabled. Constructing an emitter does not enable capture.

Every emission rechecks the current capture filter before rendering, shedding work rather than waiting for capture-control locks. The emitter
accepts closed stage/evidence enums and explicitly safe scalar fields; it never
accepts arbitrary error strings, authorization tokens, vendor payloads or station
snapshots. `DiagnosticBoundary` remains the sole serializer. Oversized identity
metadata is shed before serialization; output is capped at 64 KiB, with at most
16 additional fields. A nonblocking queue holds 1–128 records and increments a drop
counter when full or closed. A slow diagnostic receiver cannot suspend a command,
SQLite write, socket task or critical target report.

The receiver is a host instrumentation boundary, not a public stream, persistent
archive or durable event cursor. It must not be copied to normal logs or SQLite.
The shared retained ring, authenticated subscriber leases, gap reporting and export
transport are subsequent #65/#66 work. Consumers of that transport must enforce the
existing capture lifetime/read scopes again; possession of the internal receiver
is not an operator read grant.

## Evidence and causality

| Stage | What its successful evidence establishes |
| --- | --- |
| `ocpp.receive`, `validation` | A socket frame arrived; a charger CALL passed model validation |
| `application` | The ordered registration/status handler completed, rejected, or ignored a stale observation |
| `storage.commit` | An actual authoritative write returned; duplicate and failure are separate outcomes |
| `command.authorization` | The shared transport/resource grant allowed or rejected the request |
| `command.ingress`, `command.dispatch` | The coordinator received work, then invoked the live station dispatch port |
| `command.protocol_response` | Charger acceptance/rejection, proven nontransmission or uncertain outcome as classified by that port |
| `command.observed_effect` | Later explicit observed evidence was linked and persisted |
| `ocpp.send` | The socket write completed; no physical charging effect is implied |
| `target.mapping`, `target.enqueue` | A canonical delivery was routed to its exact configured destination and admitted to its bounded queue |
| `target.report` | The target reported local exposure, peer acknowledgement, failure or uncertainty |
| `management.delivery` | A command result was exposed in an HTTP response; client consumption is not asserted |

Target reports retain the distinction between local exposure and peer acknowledgement.
They do not imply charger acceptance or charging effect. The durable worker's existing
report, peer/scope and retry policy remains authoritative; diagnostics do not change
it. Canonical target routing is observed at the host ingress, not a claim that a
future adapter has mapped a vendor register or that a remote application consumed it.

Correlation is copied from commands/results and event envelopes into target work.
Missing correlation is explicitly marked `uncorrelated`, never guessed from station
identity or a nearby event. Inbound OCPP correlation includes process and connection
identity, so reusing a message ID on a reconnected socket cannot join unrelated calls.
Trace IDs and sequences belong to the emitting process; no speculative parent trace
is assigned. Existing bounded pending-call maps retain exact outgoing response links.

`IncomingCall::complete_registration` carries the socket context through both native
registration/status implementations, a per-operation `DiagnosticStore`, and the
original bounded responder. A failed handler produces no speculative successful
commit or reply. Other ordered handlers can use the same span/store boundary when
implemented, without changing protocol decisions for instrumentation.

Server-observed UTC and device `source_time` are separate fields. `duration_micros`
is monotonic elapsed time since that local span began, including its asynchronous
waits; it is not a per-stage duration or cross-device/end-to-end latency. A downstream
span can share a correlation without sharing its timing origin.

State inspection projects at most 16 resource availability enums in the ordered
handler's unchanged topology. Only changed fields are emitted as indexed before/after
values, with explicit omission metadata beyond that projection. Credentials, native
vendor text, all data points and transaction arrays are never copied into the diff.

## Verification

Application tests cover disabled/stopped/filtered capture, a saturated receiver,
uncorrelated work, oversized metadata, bounded state changes, and wildly different
device/server clocks. Real socket tests cover registration, rejection, persistence,
and state changes for OCPP 1.6J and 2.0.1. Both editions also run an authenticated
target-origin command through shared authorization, durable admission and a real
socket reply/timeout; missing replies never produce acceptance or observed effects.
The target host test checks event correlation, exact destination and absence of
invented acknowledgement at enqueue. Run `./scripts/verify-workspace.sh` for these
and the existing workspace checks.
