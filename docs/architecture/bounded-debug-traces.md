# Bounded best-effort debug traces

Explicit diagnostic capture retains one process-local ring shared by the application's
`FlowDiagnostics::retained` emitter, authenticated live readers and bounded export leases.
The service connects that emitter to its existing application diagnostics hooks. Capture remains
disabled by default and requires the enablement, permissions, filters and deadlines described in
[diagnostic capture controls](../security/diagnostic-capture.md).

## Retention and resource bounds

The ring retains at most 8 MiB of encoded JSON and 2,000 records, evicting the oldest records when
either limit is reached. Host resource limits may lower those ceilings. Every complete trace,
including identity metadata, sanitized fields and disclosure audit, is limited to 64 KiB.
`DiagnosticBoundary` counts required metadata before allocating an output buffer, checks escaped
JSON sizes before copying safe values, and uses a bounded writer. Metadata that cannot fit causes
the optional record to be dropped. Unknown vendor content is never inspected or copied into JSON.

Safe details that do not fit are omitted and set `redacted_details.truncated`. Disclosure audit
names are deduplicated, with at most 64 distinct safe field names and the four closed sensitive
classes. Both encoded bytes and the bounded audit are shared when a diagnostic is cloned.
The scalar detail fields explain omission without exposing raw source content:

| Field | Meaning |
| --- | --- |
| `details.original_size` | Sum of original rendered safe-value UTF-8 bytes and opaque vendor source bytes, before boundary omissions; excludes sensitive values, field names, JSON escaping and record metadata |
| `details.omitted_fields` | Number of safe attributes omitted by the serialization byte or field-count limit |
| `vendor_payload.original_size` | Aggregate original byte count of omitted vendor payloads, present when any were supplied |
| `audit.omitted_unknown_vendor_payloads` | Number of opaque vendor payloads omitted without inspection |

These source-size counters describe the observation supplied to the boundary. Optional details
already shed by the emitter are represented by `state_details_omitted` and the ring's
`shed_records` counter, rather than a reconstructed size of discarded input.

The ring uses the daemon's shared `RuntimeResourceBudget`: retained payload bytes and record slots
consume the capture allowance and the noncritical share of aggregate queued payload capacity.
Diagnostics cannot consume the byte reserve kept for critical work. Readers share the same record
and reservation; evicting a reader-held entry does not erase its resource accounting. Its
reservation is released when the last reference is dropped.

Producer admission uses nonblocking lock attempts. Capture-control contention, shared-budget
contention, serialization failure and exhausted capacity shed optional diagnostics and update drop
evidence. At three quarters of ring capacity or shared noncritical capacity, the emitter first
omits its additional detail fields while retaining its basic stage and correlation evidence.
No diagnostic producer waits for a subscriber or writes trace records to SQLite. The ring is
separate from durable events, command recovery and target delivery.

## Authenticated trace stream

`GET /api/v1/diagnostics/capture/{process_id}/{id}/traces` opens an SSE stream for the exact active
capture. It requires one bearer authorization header with `diagnostics:read` covering the entire
capture selection. A control-only grant cannot read traces. There are at most two live subscribers;
a third receives 429. Responses use `Cache-Control: no-store`.

The stream emits these event types:

| SSE event | Content |
| --- | --- |
| `trace_window` | Initial process/capture identity and retained-window evidence, explicitly marked `replay: "best_effort"` |
| `trace_gap` | Reason `overflow`, `dropped`, `expiry` or `slow_reader`, with available window evidence |
| `trace` | The centrally sanitized trace JSON, with a process/capture/sequence event ID |

Window evidence includes first retained and next process-local sequences, retained record and byte
counts, and cumulative eviction, producer-drop and detail-shedding counts for this capture.
Producer drops caused by contention can occur before a sequence is assigned, so drop counts also
matter when sequences appear consecutive.

Resume with one `Last-Event-ID` header containing the previous trace's JSON array ID, for example
`["process-instance",42,108]`. The optional `after` query parameter accepts the same value, URL
encoded; supplying both forms is invalid. IDs are capped at 512 bytes. Malformed IDs, duplicate
headers and sequences at or beyond the next process-local sequence receive 400. A mismatched
process returns 410 with a `trace.gap` JSON response and `restart`; a stopped, expired or different
capture returns 410 with `expiry`. Reconnection only traverses the retained window and cannot
recover evicted data or reuse a durable-event cursor.

Each subscriber has a single notification slot containing no record bytes or ring references.
One SSE frame is materialized when its response body is polled. A consumer that leaves the
notification unread for five seconds loses its lease and subscriber slot; its next body poll
receives a terminal `slow_reader` gap. Disconnect also releases the slot. Slow readers therefore
do not accumulate payload queues or keep the capture alive.

## Stop, expiry and export leases

Every lease read rechecks capture identity and lifetime and returns at most one shared record plus
constant-sized window metadata. Stop or the monotonic capture deadline immediately forbids further
live reads. A terminal `expiry` gap ends an existing stream when it next polls, and the server's
expiry worker reclaims session ownership without browser requests.

Two application export leases may be admitted while capture is active. Each can read the shared
ring one record at a time for at most thirty seconds from acquisition, including after capture
stops. These are bounded read leases, not copied capture snapshots. While an admitted export
retains a stopped session, another capture cannot start. No export can begin after stop. The last
export release or its absolute deadline releases stopped session ownership; independently held
record references remain charged until dropped. The separate
[HTTP capture-file export](../security/diagnostic-capture-export.md) route uses these leases
with stricter stop/cancellation and stalled-body cleanup. It does not change the trace SSE
route; browser import remains separate work.

## Verification scope

Application tests cover encoded-size and escaping bounds, vendor omission, bounded audits, ring
byte/count overflow, shared record reservations, selection checks, expiry, concurrent producer
pressure and preserved critical capacity. Management tests exercise authenticated router requests,
subscriber limits, window/gap events, cursor rejection, stop/restart separation and stalled-reader
lease release. A real OCPP WebSocket/SQLite registration scenario runs with two capture readers,
an overflowing ring and shared memory pressure, verifying unchanged protocol replies and durable
state while optional details are shed. These tests do not establish a runtime OCPP-to-SSE
end-to-end matrix, a soak result or Raspberry Pi performance measurements.
