# Bounded browser Debug timeline

The optional console includes a read-only Debug timeline using the existing authenticated capture
API. Opening the page or following its Debug link does not request capture, open a trace stream,
or reuse the management read credential. Enter a separate diagnostic credential and choose
**Inspect capture status** to inspect an authorized active capture. Credentials remain in tab
memory; disconnect and page departure clear them. No browser storage, cookies or third-party
telemetry are used.

**Start capture** explicitly requests a metadata or redacted-payload session for the displayed
station/target filters and duration (1–1800 seconds, default 600). Payload capture requires a station.
The server checks diagnostic permissions and rejects disabled capture, conflicting sessions and
unauthorized filters. The environment, bridge, release, selected target and station remain visible
above the controls. **Stop capture** addresses the exact process and capture ID. The console neither
extends a session automatically nor creates another command dispatch path. Charging and release
controls remain separate work; production has no replay or fault controls here.

The timeline displays stage, direction, outcome, correlation, bridge observation time and optional
device time and assurance evidence. Local exposure is never relabeled physical success. Filters
cover target instance/kind, station, protocol edition, EVSE, connector, transaction, action,
direction, severity, correlation and local-time bounds. Unsupported or absent fields remain
unavailable, including severity when the producer only supplies an outcome. Full-text search is
limited to the retained sanitized records. Metadata filters never infer absent values.

Capture station/target filters are enforced on the server before delivery. Remaining filters apply
to the bounded browser window; the current trace API has no per-event filter parameters. A filter
cannot recover events already evicted or disclose records outside the credential's capture scope.

The single browser buffer retains at most 2,000 rows or 4 MiB of encoded trace JSON, whichever comes
first. Individual records are limited to 64 KiB. Parsing produces bounded scalar indices once per
record; filtering does not reparse or sort JSON. Only ten viewport/overscan rows are mounted. Detail
JSON is expanded lazily as inert text, with at most four cached entries totaling 256 KiB. Eviction
removes cached details and bookmarks. There are at most 64 retained-window bookmarks; they are not
saved across reloads. These are encoded-data budgets, not claims about total JavaScript heap size.

Stream-driven rendering runs at five updates per second, below the ten-update ceiling. Hidden tabs
skip those updates while the same bounded buffer continues receiving data. **Pause display** pins
the visible sequence ceiling without copying the buffer, pausing capture or changing its deadline.
Older visible records can therefore disappear on eviction; eviction and expired-bookmark counts
remain explicit. Resume shows the current retained window. **Clear display buffer** clears local
rows/details/bookmarks while retaining the stream cursor, so it does not replay cleared history or
clear the service ring. Disconnect clears local data but does not stop the server's capture timer.

The authenticated stream rechecks service identity before opening/reconnecting and every 15 seconds
while connected. It uses a ten-second opening deadline, 45-second inactivity deadline, and bounded
one-to-thirty-second reconnect delay. Reconnect sends the process/capture/sequence cursor and
reports an interruption gap; trace replay is always best effort. Server overflow, drops, shedding,
expiry, and slow-reader gaps remain distinct evidence. Restart, expiry, authorization rejection or
malformed/oversized trace data requires explicit status refresh or reconnection. The countdown is
an estimate from returned remaining seconds; the server independently enforces the actual deadline.

## Verification

`npm --prefix frontend run check` tests row/byte/cache limits, paused-window eviction, bookmarks,
filters, malformed records, destination identity, cursor reconnect, gaps and terminal expiry, then
checks types, builds assets and enforces the existing 300 KiB raw / 100 KiB gzip budgets.

`npm --prefix frontend run test:browser` exercises the compiled console with an explicitly enabled
synthetic producer behind the actual Rust capture router: inert opening, scoped reader denial,
explicit start/stop, pause with autonomous expiry, detail rendering, buffer clearing, virtualization,
and desktop/mobile layout. `UOB_BROWSER_EXECUTABLE` can select an installed compatible Chrome.
The fixture producer is not included in the production daemon. These tests do not claim complete
OCPP-to-browser acceptance, soak coverage or Raspberry Pi resource qualification.

## Message and state inspection

Opening a retained row lazily builds a read-only inspector with redacted source, canonical and
target metadata alongside each other. It shows validation field paths/codes, mapping metadata,
units/quality, opaque or unsupported fields, and original-size/omitted-field indicators. JSON
highlighting uses text children only; strings containing HTML, scripts or URLs are never executed
or made into links. Nested JSON inside a scalar is kept as text. Unknown fields are explicitly
listed as unsupported, and missing sections say unavailable.

The inspector consumes the existing `redacted_details.fields` scalar map. The current safe
producer supplies protocol/action/source time, station identity, target identity, availability
changes, decision evidence, redaction markers and size information. Optional approved producer
metadata is presented using these names; absence never causes reconstruction or another request:

| Scalar field names | Presentation |
| --- | --- |
| `source.*`, `canonical.*`, `target.*` | Respective redacted representation |
| `validation.*` (for example `validation.field_path`, `validation.code`) | Literal field path and validation evidence |
| `mapping.*` (for example `mapping.topic`, `mapping.api`, `mapping.node`, `mapping.register`) | Reported destination mapping; no protocol implementation implied |
| `unit`, `quality`, or names ending in `.unit` / `.quality` | Original units and quality |
| `opaque.*`, `unsupported.*`, `redacted.*`, `vendor_payload` | Explicit opaque/unsupported/redacted evidence |
| `decision.reason`, `reason_code` | Safe producer reason, without inferred explanations |

These optional display names do not widen the server's closed safe-field allowlist. Current
production instrumentation does not emit detailed validation paths or payload mappings; those
sections remain unavailable until an approved producer supplies them. Browser fixtures exercise
that optional metadata, while real-router tests exercise the existing emitted evidence.

`resources.N.availability` values in the existing `Before -> After` format become a table capped
at 16 changes. The index is the producer's stable resource order, not an inferred connector ID.
Malformed changes remain unsupported. No full station snapshot or adjacent-event diff is built.
The inspector displays the supplied parent trace as the trigger, correlation separately, safe
reason and stage evidence. Missing parents stay unavailable. Rejection, duplicate, stale,
reconciliation and uncertain evidence receive conservative explanations without inventing effects.

The shared four-entry / 256 KiB detail cache accounts for both formatted JSON and the serialized
inspection model, removes both on row eviction or clearing, and caches formatting-limit failures.
There are at most 128 inspected fields, 4,096 characters per displayed scalar and 1,024 syntax spans
per JSON view. Unknown JSON is checked against a 32-level / 4,096-node formatting budget. Partial views are marked; the bounded raw redacted record remains available on
explicit expansion. Details never reparse or sort all timeline rows. These limits bound retained
encoded representations and rendered nodes, not total browser heap size.

See [command evidence](debug-command-trace.md) for request-scoped authorization, dispatch, response,
observed-effect and delivery evidence, safe reason codes, local links and timing limitations.
