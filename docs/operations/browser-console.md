# Optional browser console

The management adapter embeds the compiled TypeScript/React console in the Rust binary. Open the
management origin in a browser to see its bridge, environment, release and process identity before
connecting. The shell implements connection/authentication and a bounded event-connection summary.
Station/EVSE/transaction views, command widgets, configuration screens and the detailed Debug
workspace are separate backlog items.

## Raspberry Pi deployment

No Node, npm, Vite, CDN, external font, browser WebSocket or broker connection is needed on the Pi.
A normal Cargo service build includes the checked-in compiled assets. The service sends static
bytes; React runs on the operator's browser. The shell has no background animation, service worker,
IndexedDB or local/session-storage writes. It retains counters and only the latest event type,
not a growing history of event payloads.

The isolated frontend build enforces a 300 KiB total uncompressed asset budget and a 100 KiB gzip
measurement budget. The current server sends uncompressed assets; the gzip measurement is not a
claim about HTTP compression. There are only three allowlisted routes: `/`,
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

The reusable API client submits commands only with an explicitly supplied control credential,
request ID, expiry and matching bridge. It does not retry POSTs, follow a returned status URL, or
interpret HTTP 202 as charger acceptance/physical success. Command controls belong to issue #91.
Raw server errors, payloads and credentials are not logged by the shell.

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

The current `uob serve` CLI composition serves identity/health and optionally diagnostics but does
not yet attach a canonical query/event source. The shell reports unavailable on that composition,
without displaying a successful login. Hosts using the existing
`router_with_authenticated_events` or combined command/event router supply their real canonical
source and scoped authenticator. The browser acceptance fixture uses that actual router and
application with deterministic source data; it does not claim hardware charging validation.

## Build and verification

Build only on a developer or CI host, using Node 26.8.1 and npm 12.0.2:

```text
cd frontend
npm ci --ignore-scripts
npm run check
npm exec playwright install chromium
npm run test:browser
```

Direct dependency versions and transitive integrity hashes are pinned in the isolated lockfile.
`npm run check` executes transport/parser tests, TypeScript checks, the production Vite build and
asset budgets. Commit the generated `adapters/management/ui` files together with their source.
A second build should produce no asset diff. Rust release jobs consume those files and do not
install frontend tooling. Automated frontend workflow gates remain issue #97.

Browser acceptance launches three loopback-only Rust fixture servers on ports 39189–39191,
exercises the actual authenticated management router and compiled assets, forces an initial SSE
EOF then verifies durable cursor resume, and checks denied credentials, inert hostile text,
production/staging separation, desktop/mobile layout and `--no-ui` route equivalence. Fixture
credentials are public deterministic test values; the fixture is not part of the production
binary. Set `UOB_BROWSER_EXECUTABLE` to use an already installed compatible Chrome executable.

Run `./scripts/verify-workspace.sh` for the required Rust, architecture and repository checks.
