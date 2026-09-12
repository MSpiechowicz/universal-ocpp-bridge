# Browser and API diagnostics

The console's Browser and API diagnostics panel records tab-local request/failure counts,
frontend exception categories, and App/Debug committed update counts. It displays the management
stream state, last activity, reconnect attempts and durable history gaps. Inventory remains a
point-in-time query: a live stream does not make that query current.

Debug separately displays trace reconnect attempts, last trace activity, best-effort gaps,
server evictions/drops and detail shedding. Pausing freezes the retained timeline view, while
capture and deadlines continue. Hidden tabs are explicitly stale, and pause the diagnostic
panel’s sampling timer until visible. A trace reconnect or process restart cannot establish a complete history.

## Failure privacy and correlation

Each actual identity, query, control, or stream-opening HTTP request increments a saturating
counter. Non-success HTTP responses and network/deadline failures increment the failure count. Expected
caller cancellation does not. The latest failure retains only its fixed route category, numeric
HTTP status (or zero for a network failure), local observation time, and an optional server
correlation identifier. Response bodies, URLs, station names, credentials, exception messages,
stacks, filenames and rejected promise values are never copied into this diagnostics store.

Only a canonical UUID from `X-Correlation-ID` is accepted. Missing and unsupported identifiers
are visibly unavailable, rather than synthesized. “Find in retained traces” transfers that UUID
to the existing timeline correlation filter, clearing other filters. It does not fetch traces,
enable capture, submit commands, or promise that a matching record remains in the bounded window.
The current management routes need not provide this optional header; a missing header is normal.

React caught, uncaught and recoverable errors and window error/unhandled-rejection notifications
record only a fixed category and timestamp. These are observed notifications, not deduplicated
incidents. The recorder has no listeners, logging, network calls or React updates, so reporting
cannot synchronously trigger another render. The root error boundary has a static reload fallback.

## Bounds and verification

There is one latest-failure slot, one latest-exception slot, and a fixed set of counters capped at
JavaScript's maximum safe integer. The panel samples at most once per second while visible and
its own renders are excluded from App/Debug commit counts. This does not add a second trace buffer.
No browser storage or third-party telemetry is used. Management disconnect clears these observations;
reload starts a new tab-memory store.

Run `npm --prefix frontend run check`, `npm --prefix frontend run test:browser`, and
`./scripts/verify-workspace.sh`. The browser suite uses the actual Rust router for interrupted SSE,
and injects an HTTP failure with a correlation header and secret-bearing body to exercise the
optional header path. Exception-storm coverage verifies fixed-size category recording, inert
rendering and stable commit counts. Unit coverage verifies counter saturation, route classification,
header rejection and network-error privacy.
