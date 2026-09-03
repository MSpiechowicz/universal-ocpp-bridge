# Management API

The target-independent management listener exposes canonical station state when the composition
root supplies a `CanonicalQuerySource` and an authenticated `TargetQueryAuthorization`. The same
contract works with every selected target and when static browser assets are disabled.

## Routes

- `GET /api/v1/stations?limit=25&after=<opaque cursor>` returns a page with `items` and an optional
  `next_cursor`. The default limit is 25 and the application maximum is 100.
- `GET /api/v1/stations/{station_id}` returns one canonical `StationSnapshot`, or `404` when the
  authorized source has no matching station.

Callers must treat cursors as opaque. Invalid limits or cursors return a sanitized `400` response.
The scoped application query port filters inventory before pagination and rejects source responses
outside the authenticated resource grant.

Each router has a host-owned concurrency cap and per-query deadline (eight concurrent reads and two
seconds by default). Excess concurrency returns `429`; a slow source returns `504`. These bounds
keep management reads from waiting on charging work indefinitely. If no canonical source is wired,
station routes return `503` while health, readiness, metrics, identity, and optional static assets
remain independently available.

## Commands

`POST /api/v1/commands` accepts the versioned canonical `CommandRequest` JSON contract. The HTTP
authentication layer supplies the trusted management principal separately from the body, and the
composition root wraps the common application admission port with that principal's control and
resource grant. Privileged OCPP operations additionally pass through the protocol adapter's pinned
action/schema registry before admission.

The endpoint returns `202` only when the common application path reports a durably admitted command.
Its response includes the request ID, `/api/v1/commands/{request_id}` status URL, and the latest
result. `GET` on that status URL reads the common canonical result, so management, CLI, and target
consumers observe the same lifecycle. Charger protocol acceptance remains in `lifecycle`; later
physical charging evidence remains in the separate `observed_effects` collection.

Invalid input and conflicting request IDs return `400`/`409`, missing permission returns `403`, an
expired request returns `410`, unsupported capability returns `422`, bounded saturation returns
`429`, and unavailable authoritative persistence returns `503`. A storage commit failure therefore
cannot be mistaken for durable acceptance. Stable response bodies contain an `error` code and no
credential or raw internal failure details.

Health responses distinguish core readiness from selected-target, broker, client, and optional
export degradation. They contain only stable component reason codes and counters; credential
material and transport error details do not enter the management schema.

## Durable event stream

`GET /api/v1/events` is enabled only when the composition root supplies both a canonical query
source and an adapter-owned bearer authenticator. Authentication is required on loopback as well
as remote listeners. The authenticator resolves the token outside application/storage code and
returns an immutable read permission, canonical resource scope, and exact default resource. The
default lets `uob events --format jsonl` subscribe without adding resource parameters to its
configured endpoint. A valid stream grant includes both retained-event and station-snapshot
permissions, covers the selected resource, and covers its containing station. The route rejects an
event-only or exact-child-only grant before opening the source because that credential could not
perform the mandatory cursor-gap recovery. This does not widen the event filter: every emitted
record must still match the one selected resource exactly.

Clients may select one exact canonical resource with `station_id` and optional `evse_id` and
`connector_id` parameters. A connector without `evse_id` is an OCPP 1.6-style connector; a
connector with `evse_id` is an OCPP 2.0.1 EVSE child. `types` accepts at most eight comma-separated
exact event types. Identifiers, cursor values, filter counts, and filter lengths are bounded before
the retained-event source is opened. The authenticated scope is checked on the request and again
on every source item, so a faulty source cannot leak an event for another resource.

Every replayable record uses SSE event name `durable`, contains the canonical `EventEnvelope` as
JSON data, and uses the exact opaque `uob:event:` checkpoint following that record as its SSE
`id`. It never derives the checkpoint from `EventId`, the resource-local sequence, a telemetry
sequence, or a diagnostic trace ID. Browsers may reconnect with `Last-Event-ID`; non-browser
clients may use `after`. Supplying both with different values is rejected. Type-filtered records
advance the connection with an ID-only checkpoint, avoiding repeated scans without exposing their
payloads. Best-effort telemetry and diagnostics are not carried by this durable route.

If a cursor is already outside retention when the request opens, the server returns `410` with
`error: events.cursor_expired`, `kind: durable_cursor_gap`, and a recovery object. If retention is
lost after response headers, the stream emits a named `gap` event without an `id` and closes.
Recovery is deliberately the same in both cases: use the same bearer credential to fetch the exact
station snapshot URL in the recovery object, replace local state, then use its resubscribe URL
without the expired cursor. That URL preserves the station, EVSE, connector, and exact event-type
selectors from the failed subscription. The server never silently advances across the gap.

Event producers, source subscriptions, HTTP queues, and serialized envelopes are bounded. The
event-specific ceiling is 32 producers (the shared process subscriber budget may lower it), each
subscription requests at most 32 source records, an envelope is capped at 256 KiB while it is
serialized, recovery/error signals are serialized through a separate 16 KiB hard bound, keep-alive
comments are sent every 15 seconds, and an HTTP queue that remains full for five seconds is
disconnected. Each connection holds one shared application subscriber reservation whose byte count
tracks the actual encoded size of all queued SSE records. Dropping the client cancels even an idle
source poll and releases both admission slots, so slow readers cannot retain unbounded memory or
admission capacity.

The combined router constructor does not reuse an event bearer for command submissions. Command
authentication and trusted origin remain the independent host-configured command boundary.
