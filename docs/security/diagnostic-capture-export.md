# Bounded diagnostic capture export

`GET /api/v1/diagnostics/capture/{process_id}/{id}/export` downloads the currently
retained redacted capture as UTF-8 JSON Lines. It uses the same independently authenticated
management listener as capture controls. Capture must be explicitly enabled and active; the
bearer credential needs `diagnostics:read` over the **entire** capture's bridge, station and target
selection. A capture-control permission alone does not permit export. The route never starts
capture, expands its filters, writes a server-side file or reads raw protocol payloads.

Admission failures use the existing `capture.*` error codes: 401 for invalid credentials,
403 for insufficient scope, 410 for an old process/capture or inactive session, and 429 when
export capacity or bounded provenance metadata is exhausted. Responses use
`application/x-ndjson`, an attachment filename, `Cache-Control: no-store` and `nosniff`.
No credentials, endpoint addresses, secret references or full configuration files enter the
provenance. Its configuration allowlist contains only capture level and memory-only persistence;
bridge/environment/release/process/selected-target identity comes from the host's existing
public `ServiceIdentity` contract.

## Version 1.0 file contract

The checked-in [line schema](../../adapters/management/schemas/capture-line-v1.schema.json)
uses JSON Schema Draft 2020-12 and references the published canonical service-identity and
trace-record schemas. Tests register those dependencies locally and validate emitted lines offline.
A successful finite download has exactly this order:

1. One `manifest` line with capture schema version `1.0`, canonical trace schema version,
   adapter build version, service identity (including release digest), capture ID and filters,
   sanitized configuration context, byte/record/time limits and the initial retained window.
2. Zero to 2,000 `trace` lines. Each wraps the existing centrally serialized diagnostic as
   `{"type":"trace","record":...}` without reconstructing or widening its safe fields.
3. One `summary` line with termination reason, exported count, bytes before the summary,
   last exported sequence, unexported initial records, missing sequence count, truncated-record
   count and the last observed ring window.

The initial window fixes an **exclusive next-sequence cutoff**. Later arrivals are never exported.
This is a traversal of the live retained ring, not a snapshot: producers may evict initial records
during the download. A positive `window.evicted_records` explicitly reports prior overflow;
`dropped_records` also covers drops before sequence assignment, and `shed_records` reports
optional detail shedding. `unexported_initial_records` reports initial records the download
could not export, including eviction during traversal. `missing_sequences` counts skipped
sequence positions before exported records inside the initial range, not drops that occurred
before that range or after its last exported record.

Both manifest and summary always say `history_complete: false`; replay is best effort.
`retained_window_complete: true` means only that every record retained at admission was exported.
It does not erase prior overflow, producer drops, or omitted/redacted payload detail.
`truncated_records` counts exported records whose centrally encoded `redacted_details.truncated`
flag is set. Individual records retain source-size and omission metadata. The last observed
window can include later arrivals in its counters; those records remain outside the export cutoff.

The summary's `reason` is `window_end`, `byte_limit`, `record_limit`, `timeout`, `slow_reader`,
`capture_stopped_or_expired`, or `permission_revoked`. Only `window_end` can report a complete
retained window. A missing manifest or summary, or a partial JSON line, means the file is
incomplete. A stalled socket may close before its client receives a summary. Clients must not
interpret EOF alone as success. If authorization or lifetime ends before the first body poll,
only a terminal summary is produced, with no identity-bearing manifest or records.

Readers must enforce the documented byte/line/record limits before parsing, treat payloads as inert
untrusted data, and reject unsupported major versions. Importing a file must not execute content
or automatically issue commands. The offline browser inspector remains separate backlog work.

## Resource ownership and cancellation

The service admits at most **two concurrent exports**, independently of its two SSE subscriber
slots. Each export uses one revocable ring lease, bounded scalar progress state and a provenance
line of at most 16 KiB. There is no complete capture copy or payload queue. A response poll reads
at most one shared record and builds one chunk of at most 64 KiB plus its small JSONL wrapper;
no record reference survives the poll. Total response bytes, including manifest/wrappers/summary,
are capped at **9 MiB**, with 16 KiB reserved for the final summary. The 8 MiB / 2,000-record ring
and central 64 KiB record ceiling remain unchanged.

The absolute download lifetime is **30 seconds** and unread-body idle time is **5 seconds**.
Progress cannot extend the absolute deadline. A watchdog runs every 100 ms independently of
body polling, drops the lease on timeout, capture stop/expiry or permission revocation, and
releases the export slot, pending provenance and credential header. It reauthenticates the credential and rechecks the entire current
selection; each body poll repeats those checks before exposing another record. This HTTP
export ends at capture stop, even though the lower-level application export lease can retain
a stopped capture for its bounded lifetime. An unread response cannot prevent ring expiry.
Cancellation drops the lease synchronously and aborts the watchdog. Completion and limits also
release it before returning the final summary, even if the body is never polled again.

Only a bounded credential header is retained for permission rechecks; it is never included in
output or diagnostic logging. Normal server/socket buffering may retain already emitted bytes;
revocation prevents subsequent reads rather than recalling bytes already sent. The stream yields
between chunks so a fast client cooperates with other service tasks.

## Verification

Management tests exercise route authorization, station/target scope changes during a download,
central secret/vendor omission, process/capture identity, empty and overflowing windows, fixed
cutoffs under new arrivals, bounded metadata, byte/record termination, concurrent admission,
cancellation, and unpolled timeout/stop memory reclamation. The tests validate file lines against
the checked-in schema and inspect the shared runtime's retained-record accounting. They do not
claim Raspberry Pi performance or browser-import coverage.
