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
