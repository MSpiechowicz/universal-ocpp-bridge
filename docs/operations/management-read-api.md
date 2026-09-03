# Management read API

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

Health responses distinguish core readiness from selected-target, broker, client, and optional
export degradation. They contain only stable component reason codes and counters; credential
material and transport error details do not enter the management schema.
