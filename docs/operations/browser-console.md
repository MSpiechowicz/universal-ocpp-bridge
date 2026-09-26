# Optional browser console

The management adapter embeds the compiled TypeScript/React console in the Rust binary. Open the
management origin in a browser to see bridge, environment, release, process and selected-target
identity before connecting. An opt-in, demo-only charging listener can supply durable station
inventory, connector/EVSE topology, transactions, typed observations and explicit capabilities.
With separately provisioned command grants and station opt-ins, the console also submits supported
start, stop, charging-limit and schema-validated privileged actions. The [bounded Debug timeline](debug-timeline.md)
retains its separate diagnostic capture and trace inspection boundary. Configuration and release
controls remain separate work.

## Raspberry Pi deployment

No Node, npm, Vite, CDN, external font, browser WebSocket or broker connection is needed on the Pi.
A normal Cargo service build includes the checked-in compiled assets. The service sends static
bytes; React runs on the operator's browser. The shell has no background animation, service worker,
IndexedDB or local/session-storage writes. The normal event summary retains counters and only the latest event type. Debug separately retains
a bounded sanitized trace window when explicitly connected.

The isolated frontend build enforces a 368 KiB total uncompressed asset budget and a 109 KiB
gzip measurement budget for the three allowlisted assets. Browser command controls increased the
measured footprint beyond the former 352/108 KiB ceiling. The Rust asset test separately bounds
each served response; the server sends uncompressed assets, so gzip is not an HTTP compression claim.
There are only three allowlisted routes: `/`,
`/ui/assets/console.js` and `/ui/assets/console.css`. Responses are `no-store` with explicit MIME,
`nosniff`, no-referrer and a restrictive same-origin content security policy. Unknown asset paths
are 404. Assets are not read from disk at runtime.

`uob serve --config bridge.toml --no-ui` disables all three routes while preserving management API
and configured event/capture streams. No compiler or frontend process is launched by Cargo or the
production daemon. The full x86-64/ARM64 product-package qualification remains issue #175; this
change embeds architecture-independent browser assets in the existing service build.

## Authentication and environment separation

Use the console on the same HTTPS origin as its management API. HTTP is accepted only on literal
loopback/localhost for local access, development or an appropriately configured SSH tunnel. The
console does not add remote-listener enablement or weaken the existing TLS policy. Do not use a
Vite development server or an unauthenticated reverse proxy to expose the production API.

Deploy production, staging and demo on distinct origins as required by the environment policy.
For example, use `https://uob.example`, `https://uob-staging.example`, and
`https://uob-demo.example`, each terminating TLS at its own management listener with independent
credentials. For local SSH tunnels use separate ports (for example 8080, 8081, 8082). A different
path on one origin is insufficient. Never reassign a live production origin to a test instance.
Keep staging's existing isolated network namespace and listener restrictions in force.
There is no cross-origin URL selector, shared credential storage, cookie authentication, redirect
following or automatic login. Enter a scoped management read credential explicitly in each tab.
The input is cleared on submission; the credential lives only in that client's memory. Disconnect,
reload and page departure abort requests and clear it. Browser history restoration reloads the
page. Browser/password-manager behavior is outside the application; the form requests no autofill.

Before each authenticated query or stream reconnect, the client rechecks the public service
identity without a credential. Changes to bridge, environment, release, digest, process or selected
target invalidate the session. A connected stream also rechecks identity every 15 seconds. Distinct
origins are still required: client identity checks cannot make reassigning one live origin between
production and staging safe against races. Browser permission checks supplement server checks;
they never grant access. Server data is rendered through React text nodes, never HTML injection.

Diagnostic token provisioning now binds the complete bearer secret to the configured environment;
see [capture credential migration](../security/diagnostic-capture.md). Host compositions supplying
management read/command authenticators must likewise provision independent complete tokens for each
origin/environment and validate their audience with `token_matches_environment` before verifying the
**entire** secret and its normal scoped grant. This helper checks syntax/audience only and never
supplies command authorization. Custom external token verifiers must enforce an equivalent audience
binding. Host-configured command authentication remains required by `ManagementCommandConfiguration`.

Every browser mutation requires confirmation of the visible origin, environment, bridge, release,
and selected target. Command submission additionally names the station and requires fresh confirmation
for each submission, including an intentional retry. Capture start/stop consume the checkbox
confirmation even on failure; status inspection needs no confirmation. The common client rejects
mutations without a matching destination key and rechecks live identity before sending credentials.
Browser permission checks are supplementary: the service independently authenticates each grant,
resource and operation. Release controls are not supplied by these charging grants.

The command panel takes a separate control or privileged credential for protected options and each
submission; neither the read token nor a station WebSocket credential authorizes commands. It
submits an immutable request ID, correlation ID, expiry, bridge and resource, and never automatically
retries a POST. HTTP 202 confirms admission, not charger acceptance. If the response is unknown,
check the request ID before any intentional retry; retry only the exact unexpired request with fresh
confirmation and credential. A different body under that request ID is a conflict. Raw server
errors, payloads and credentials are not logged by the shell.

## Connection and stream behavior

The initial authenticated query reads one inventory page of at most ten stations. The number shown
is a query result, not a continually refreshed station inventory. An optional station field scopes
the SSE request; leaving it empty uses the server credential's default resource. Opening the shell
never starts a diagnostic capture or submits a command.

The client admits at most two concurrent data queries, plus its periodic identity probe, with a
five-second JSON deadline and a 1 MiB response limit. It runs one SSE connection per connected tab, with a ten-second opening deadline
and a 75-second inactivity deadline (the server supports keep-alives up to 60 seconds). Parsing is
incremental, accepts fragmented UTF-8/CRLF, caps a pending record at 264 KiB and a cursor at 512
characters, and processes id-only cursor checkpoints. UI updates occur at most once per second.
Hidden tabs explicitly show potentially stale display state when visibility changes; no unbounded
buffer waits for the tab to become visible.

EOF or network failure marks the stream stale, then reconnects with `Last-Event-ID` using delays
from one to thirty seconds. A history gap fetches the authorized station snapshot before clearing
the expired cursor, and keeps a visible gap count. It does not claim the snapshot restores missing
event history. Authentication failure, changed identity or malformed/oversized transport stops the
connection and requires an explicit reconnect. A keep-alive only proves stream activity, not a
charger action or the freshness of an earlier inventory response.

An API-only `uob serve` configuration still returns 503 for station reads and events: the service
does not invent charger state. An explicitly configured demo charging listener instead composes
the private SQLite operational store with bearer-authenticated, roster-scoped station inventory,
detail and durable SSE. Reads are limited to configured station identities, including SQL
pagination before LIMIT; connector-only grants cannot enumerate whole stations. Each selected
station uses a separate stream cursor. Selection, EOF, a cursor gap or reconnect marks existing
observations stale until the relevant page and detail have been queried again. Connectivity
changes also emit a scoped invalidation event; the browser re-queries the full snapshot rather
than treating the event as one. A retained Debug link preselects only the station filter: transaction,
EVSE, connector and point identifiers are not indexed in retained traces, and a matching trace is
not guaranteed. Opening Debug never starts a capture. Unsupported operations stay unadvertised.

The demo transport uses a separate loopback WebSocket listener and must be enabled explicitly.
Read, control and privileged credentials are independent; per-station opt-ins and per-resource
capabilities still constrain commands. Start additionally needs a provisioned, protected local
authorization identity. The panel exposes only advertised operations and server-pinned privileged
schemas, validates their fields, and never treats a schema as a permission grant. Its station-scoped,
server-backed history and request detail separate admission, dispatch, native protocol response
and later linked observed effects. Event IDs link durable station/resource observations, while a
correlation link filters retained Debug diagnostics; missing traces do not prove an outcome.
Start, stop and availability may produce linked transaction/availability events only when the
station later reports them. A pending native transaction may receive a transaction-bound
`TxProfile` charging limit, but neither that state nor an accepted charging profile proves power
flow. Protocol acceptance is not observed charging success.

This mode is **not** a production plaintext charging listener or production command exposure.
See the [headless configuration guide](headless-cli.md#demo-charging-station-views).

## Build and verification

Build only on a developer or CI host, using Node 26.8.1 and npm 12.0.2:

```text
cd frontend
npm ci --ignore-scripts
npm run check
cargo build --locked -p uob-service --bin uob --example charging_browser_peer
cargo build --locked -p uob-management-adapter --example browser_fixture
cargo build --locked -p uob-sim --bin uob-sim
npm exec playwright install chromium
npm run test:browser
UOB_LIVE_BROWSER=1 npm run test:browser
```

Direct dependency versions and transitive integrity hashes are pinned in the isolated lockfile.
`npm run check` executes transport/parser tests, TypeScript checks, the production Vite build and
asset budgets. Commit generated `adapters/management/ui` files together with their source; a
second build must produce no asset diff. Rust release jobs consume those files without installing
frontend tooling. The [frontend checks](../testing/frontend-checks.md) build the daemon and
authenticated OCPP peer and run both router-fixture and real-daemon browser suites.

The router fixtures on ports 39189–39192 cover scoped authentication, cursor resume, inert
rendering, navigation and layout. The API-only daemon on 39193 proves the honest 503 path.
The separate real daemon uses private SQLite/files, independently authenticated read/control/
privileged grants, two OCPP editions and multi-EVSE traffic on 39195–39196. Its command scenario
exercises rejected and expired requests, exact retry deduplication, start/limit/stop/availability
protocol replies and later linked station/resource events without intercepting management responses:
`UOB_LIVE_BROWSER=1 npm run test:browser -- live-command.browser.ts` from `frontend`.
The broader live suite also verifies read/SSE isolation, topology, value quality, transaction
observations and disconnect/reconnect behavior. Set `UOB_BROWSER_EXECUTABLE` to use an installed
compatible Chrome executable.

Run `./scripts/verify-workspace.sh` for the required Rust, architecture and repository checks.
