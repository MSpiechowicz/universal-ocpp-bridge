# OCPP call lifecycle

The authenticated `StationConnection` has one socket-owning call-session task. It multiplexes
charger frames, bridge-originated calls, application responses, deadlines, and shutdown in one
bounded `tokio::select!` loop. Callers receive an awaitable result handle, so waiting for one charger
reply never stops the task from reading another station frame.

## Validation and direction

Text messages must be an OCPP JSON array with the exact shape for `CALL`, `CALLRESULT`, or
`CALLERROR`. Message identifiers must be nonempty, actions and error codes must be strings, and
payload/detail positions must be objects. Charger-originated calls then pass through the negotiated
1.6J or 2.0.1 typed model adapter before they reach application logic. This rejects malformed
payloads, unknown actions, and actions without a reviewed charger-to-application mapping.

Malformed frames with a usable message ID receive a sanitized `CALLERROR` containing a stable code
and, when known, a JSON Pointer path. Invalid JSON without a usable identity is recorded as bounded
diagnostic evidence but cannot be given a fabricated correlation identity. Binary data messages
terminate the JSON session. The WebSocket endpoint separately rejects messages above the configured
byte bound before parsing.

Remote `CALLERROR` descriptions and arbitrary detail members are not propagated. The lifecycle
retains only a recognized OCPP error code (mapping unknown values to `GenericError`) and a valid,
bounded JSON Pointer path. This prevents peer-controlled diagnostic text or secrets from crossing
the adapter boundary.

## Correlation and outcomes

Each bridge-originated call supplies a unique OCPP message ID and application correlation ID. The
session holds a shared `PendingRequest` resource reservation until one of these terminal outcomes:

- a matching `CALLRESULT` with its object payload;
- a matching sanitized `CALLERROR`;
- a response deadline, recorded distinctly so a later reply is diagnosed as late;
- a proven pre-transmission rejection such as invalid input, duplicate ID, or exhausted capacity;
- write failure, disconnect, or session cancellation, which is conservatively transmission
  uncertain.

The adapter never retries a timed-out or uncertain call. The application command coordinator maps
that classification into its durable command lifecycle and requires observed-state reconciliation
before any later action. Matching is independent of arrival order. Unmatched responses are bounded
diagnostics and never invoke application behavior.

Recent incoming and outgoing message IDs are retained in bounded histories. Reuse is rejected while
an incoming call awaits its application response and for the recent-history window after completion;
outgoing IDs also remain reserved across timeout and late-response handling. This prevents a stale
reply from completing a newer command.

### Native device-model report routing

Opt-in OCPP 2.0.1 GetBaseReport/GetReport register a bounded NotifyReport route
with this same socket owner before enqueueing the outbound CALL. No second
reader is opened. CALL unique IDs correlate native acknowledgements; native
signed 32-bit `requestId` values independently identify the shared NotifyReport
namespace across both actions. Station and actual connection generation come
from authenticated ownership, not the notification payload. IDs are unchanged,
retired in a budgeted set capped at 4,096 per connection and reusable only after
teardown. Reuse/exhaustion is rejected before wire.

The owner signals the actual dispatch-start instant used by the nonrenewing
30-second report deadline. Native reply timing and fragment arrival do not
restart it. NotifyReport validates the whole native frame and authorized scope,
reserves bounded ingress before sanitization/copying and hands fragments to the
registered collector. Queue/byte/capacity failure terminalizes that collection
without blocking heartbeat, status or charging handling. Default composition
shares four report slots and a 16 MiB queue budget, protects critical reserve and
enforces the 256 KiB complete-frame cap.

A schema-valid NotifyReport may receive its empty native CALLRESULT even when
late, unsolicited or unable to enter a full collection queue. That response is
wire acknowledgement, not durable inventory completion or permission to mutate
finalized evidence. Reports received before an Accepted native acknowledgement
remain private/pending; negative or missing acknowledgement never becomes success
because fragments arrived. Connection-owned collection survives an initiating
HTTP timeout or abandoned client; disconnect ends the route, never queues it
for a replacement socket. See [device-model semantics](ocpp201-remote-control.md#opt-in-read-only-device-model-queries).

## Application response boundary

A validated charger call is delivered with a single-use responder. Application work may complete in
any order while the session continues reading. The responder accepts only an object result or a
sanitized adapter-owned `OcppCallError`; a missing or invalid application response becomes
`InternalError`. Incoming and response queues are bounded, and admitted payloads hold shared runtime
reservations until consumers release them.

## Verification

The authenticated raw hostile peer runs against the real Axum endpoint for both `ocpp1.6` and
`ocpp2.0.1`. Acceptance tests cover invalid JSON, malformed shapes and field paths, unknown actions,
duplicate IDs before and after completion, two charger calls delivered before either response,
out-of-order results/errors, timeouts and late replies, disconnect uncertainty, and oversized
messages closed before application delivery. The independent peer records only payload sizes and
SHA-256 digests rather than retaining payload content.
