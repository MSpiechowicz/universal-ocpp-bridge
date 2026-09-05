The external client initiates HTTPS requests and an authenticated HTTP SSE subscription to the
selected integration listener. No webhook, MQTT broker, outbound callback, management API,
or browser session is required. Plain HTTP is restricted to explicitly isolated demo/test use.

Capabilities, OpenAPI and schemas are readable without authentication only on a credential-free
loopback listener. That default grants no access to canonical state or commands. With a credential
file, every one of these resources requires its integration bearer token. Read permission and
station/resource scope are checked for every read and stream; control permission is required for
command admission and status. Privileged OCPP operations are refused on this integration surface.

Clients must obey the runtime limits returned by capabilities. List pages default to 25, reject
zero/oversized limits, and return an opaque next_cursor when more scanning is needed. An empty
page can still have a cursor after scope filtering. Reuse a cursor only with its original endpoint
and filters; point pages are live views, not immutable snapshots. Point values preserve units,
exact decimal strings, timestamps, quality and freshness. Missing values never mean zero.

A 202 command response means durable admission (or replay of a known result), not charger
acceptance or physical completion. Follow status_url and inspect the canonical lifecycle and
separately linked observed effects. Retry an uncertain HTTP outcome with the same request_id and
identical payload; do not invent a new idempotency identity. Authentication supplies origin and
target identity; clients cannot submit them. A typed rejected CommandResult may be returned instead
of the small integration error object; both forms are described for command failures.

SSE durable records use event: durable, id: <opaque cursor>, and JSON EventEnvelope data.
Only durable records have replay IDs. Supply after or Last-Event-ID (both must agree if supplied).
Without a cursor, subscription starts from the retained source's default position. Cursors are
resource-scoped and must not be reused as list cursors. Expiry before streaming returns HTTP 410
CursorGap. Expiry during streaming emits event: gap with CursorGap data and closes; other terminal
failures emit event: error with IntegrationError data. Fetch recovery.snapshot_url and resubscribe
using recovery.resubscribe_url without the expired cursor. A fresh snapshot and resubscription
are not an atomic historical replay. Keep-alive comments occur every 15 seconds. Slow readers
are disconnected at the advertised bounded queue/deadline; reconnect using the last consumed ID.
Telemetry is best effort and absent from this durable stream.

Target acknowledgement means local exposure through this API, not consumption by an EMS peer.
An absent or slow subscriber does not hold the target outbox or block local charging. Durable
replay lasts only within retention. Stable error objects contain a namespaced error code and never
echo submitted credentials or payloads. Unknown paths return 404 and unsupported methods 405;
Axum automatically supports HEAD on GET routes with identical headers/status and an empty body.
