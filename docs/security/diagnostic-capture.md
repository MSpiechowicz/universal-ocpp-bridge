# Explicit scoped capture sessions

Capture is disabled by default in every environment, including production. Opening the browser,
reading health, and requesting capture status do not start collection. Enable it deliberately in
service configuration and provide separate diagnostic grants:

```toml
[diagnostics]
allow_capture = true

[[diagnostics.credentials]]
token_file = "/run/uob/diagnostics/operator.token"
permissions = ["diagnostics:read", "diagnostics:capture"]
stations = ["station-a"]
targets = ["mqtt-main"]
```

Credential files contain `uob1.<environment>.<secret>`, where environment is exactly
`production`, `staging`, or `demo` and secret is a unique random value of 32–128 printable ASCII
characters, optionally followed by one newline. Generate an independent secret for each environment.
Startup rejects a token whose environment differs from the configured service identity. The entire
token is compared in constant time; changing its environment label does not grant access. Legacy
unqualified token files must be rotated to this format before upgrading. Use a private file readable by the service account, with no
world permissions, symlinks, or additional hard links. Configuration checking validates references
without reading them; startup and production `config check --secrets` resolve them. Duplicate
paths and duplicate resolved secrets fail closed. No credential material appears in responses or
capture state. At most 32 credentials and 128 identities per scope list are accepted.

An omitted station or target list explicitly grants all identities in that dimension within the
configured bridge; an empty list is invalid. Restricted credentials must request a filter they
fully cover. A station-A reader cannot inspect a session for station B, or an unfiltered session.
Target restrictions apply independently. `diagnostics:read` permits inspection, streaming and
export leases; `diagnostics:capture` permits start, stop and extension. Neither permission grants
ordinary charging commands, and ordinary read/control permissions do not grant diagnostic access.
Keep demo and staging credentials separate from production credentials.

## HTTP controls

All routes require exactly one `Authorization: Bearer …` header; cookies and query tokens are
not accepted. Control bodies are capped at 4 KiB and reject unknown fields.

| Method and path | Operation |
| --- | --- |
| `POST /api/v1/diagnostics/capture` | Start an explicit session |
| `GET /api/v1/diagnostics/capture` | Read authorized active status without extending it |
| `GET /api/v1/diagnostics/capture/{process_id}/{id}/traces` | Stream the authorized retained trace window over SSE |
| `POST /api/v1/diagnostics/capture/{process_id}/{id}/extend` | Explicitly set a new deadline |
| `POST /api/v1/diagnostics/capture/{process_id}/{id}/stop` | Stop the exact session |

Start body:

```json
{"station_id":"station-a","target_id":"mqtt-main","level":"redacted_payload","duration_seconds":600}
```

The default level is `metadata`; `redacted_payload` requires a station filter and permits only
payloads that have crossed the existing central redaction boundary. There is no raw level.
The default duration is ten minutes; start and explicit extension accept 1–1800 seconds.
Extension takes `{"duration_seconds":600}` and cannot alter the level or filters. Stop has no body.
Status/start responses contain the trusted environment, bridge, release, selected target and
process identity, session ID, exact filters and remaining monotonic seconds. Use that process ID
in control URLs: an old process URL cannot mutate a session after restart. Responses are not
cacheable. Missing/invalid credentials return 401, forbidden/disabled access 403, concurrent
capture 409, invalid controls 400, and stopped/expired sessions 410.

## Lifetime and sink boundary

One manager is shared by the service's diagnostic controls and sink integrations. One active
capture is allowed. A second start returns a conflict, even with the same filter. A single lazy
server worker reaps expired session state every 100 ms without browser requests; all reads and
collection checks enforce the exact monotonic deadline. Browser disconnect, paused rendering,
status polling and wall-clock changes cannot extend capture.

The application provides a cheap selection/level check before formatting. Live readers and export
consumers must acquire a read-authorized lease for the complete selection and check each record
through that lease. There are two live subscriber slots and two export slots. Subscribers lose
access immediately at stop/expiry and cannot retain session ownership. Exports already admitted
may finish for at most thirty seconds from acquisition, including after stop. No new exports can
start after stop. A retained export keeps the single session slot occupied until release or its
absolute deadline, preventing overlapping capture allocations. Stale leases cannot read a later
session. Dropping the last export releases stopped session state; abandoned exports are reaped
without further requests.

The existing diagnostic hooks feed one shared ring bounded to 8 MiB / 2,000 records, with at most
64 KiB per centrally sanitized record. Live SSE reports retained-window, gap and trace events;
slow readers lose their subscriber lease after five seconds without consuming notifications.
Every read rechecks the capture lease and returns at most one shared record. See
[bounded best-effort debug traces](../architecture/bounded-debug-traces.md) for shared resource
accounting, reconnect cursors, omission metadata and exact stream semantics. Application export
leases provide bounded access to this same ring; the
[HTTP capture-file export](diagnostic-capture-export.md) adds finite JSONL downloads with
independent deadline, stop, permission and cancellation checks.
No trace replay is durable, and a stale lease cannot read a later capture.

## Verification

Application tests exercise independent permissions, station/target isolation, immutable filters,
concurrent starts, two-subscriber limits, bounded exports, stale IDs, and autonomous expiry.
Management tests exercise real router requests, production-disabled behavior, inert monitoring,
process-bound control paths, and expiry after the initiating response has been dropped. Service
tests verify offline configuration and private startup credential resolution.

See [bounded capture-file export](diagnostic-capture-export.md) for the authenticated JSONL
download, versioned provenance, explicit gaps, and download resource limits.
