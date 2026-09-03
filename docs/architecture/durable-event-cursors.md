# Durable event cursors

Critical business events use the application-owned retained-event port. Each query selects one
canonical `ResourceRef`, supplies a bounded page size, and may continue after an opaque
`RetainedEventCursor`. The SQLite adapter orders these events by committed journal position and
returns a checkpoint after the last event in every nonempty page.

The checkpoint remains available at the current live end. A consumer persists it only after it has
processed the returned events, then supplies it after a reconnect or process restart. An empty read
at that position returns the same checkpoint; events committed later are returned after it. This
avoids skipping an event that arrives after a consumer reached the earlier end of the stream.

Before resuming, storage verifies that the cursor still identifies a retained row in the exact
resource stream. A deleted retention position, a cursor from another resource, or an invalid
adapter position returns `StorageErrorCode::CursorExpired` with an instruction to fetch a fresh
authoritative snapshot. Storage never silently advances an expired consumer to the oldest retained
event or the newest event.

Durable event cursors use the `uob:event:` namespace. Incremental export cursors, telemetry
sequence values, process-local debug `TraceSequence` values, MQTT packet identifiers, and SSE
transport identifiers are not accepted by this port. Telemetry and traces remain best effort and
do not inherit journal retention or replay guarantees.

Application-owned retained subscriptions pair every emitted envelope with the exact opaque cursor
immediately after that envelope. This per-item checkpoint is required by transports such as SSE:
using only a page-end cursor could skip records when a connection closes mid-page. The management
adapter places that cursor in SSE's `id` field and accepts it back through `Last-Event-ID`; it does
not reinterpret the envelope's globally unique `EventId` or resource-local `sequence` as a storage
position.
