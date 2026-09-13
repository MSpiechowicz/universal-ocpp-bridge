# Debug connections, resources and export

Expand **Show connections, resources and export** in Debug to read the existing
public management health snapshot. No diagnostic credential, capture, command,
provider connection or exporter task is started. Capture credentials remain
separate and are needed for the local commit correlation link in the timeline.

The panel verifies the process/environment identity around each bounded read.
It polls at most once ten seconds after the previous read completes, with no
overlapping reads or catch-up queue. Hiding the panel, hiding the tab or leaving
the page cancels outstanding reads. A failed refresh marks retained data stale;
a changed identity clears it. A health HTTP 503 can contain a valid unready-core
snapshot; malformed 503 bodies remain errors and raw errors are never rendered.

The view distinguishes local storage safety and retained `storage.commit` trace
evidence from exporter connectivity and explicit remote committed-record counts.
The latter are cumulative observations, not a claim that every newly enqueued
record has arrived. A reconnecting exporter can therefore coexist with safe local
storage and previously committed remote records. Target acknowledgements never
become database commit evidence. Paused capture uses its existing sequence ceiling;
cleared or evicted traces cannot supply commit evidence.

Queue use, capacity, drops, required delivery backlog, logical storage usage,
database operation latency, daemon RSS and cumulative CPU time come from health
observations. Zero remains visible; absent, malformed or inexact JavaScript integer
values say **Unavailable**. CPU time is not a utilization percentage. Logical
storage use is not physical SQLite file size. Core readiness does not establish
an individual successful transaction. Snapshot receipt is not producer freshness.

Offline validated target kind/capability metadata is copied at service composition
without creating an adapter. Capabilities are registry declarations, not evidence
that a charger supports an operation. At most 32 safe labels are retained; invalid
or oversized metadata is unavailable.

`HealthMonitor::report_export_observation` is a passive producer port for safe
provider/destination-revision labels, the closed set of selected record classes,
enqueue/remote-commit counts, batch success/failure/retries, lag, duplicates,
quarantines and gap/drop counts. It retains one bounded observation plus its
monotonic age; observations older than 30 seconds are labeled stale. Null values
mean unobserved; disabling export clears the prior observation. Producers must replace or clear it on destination/revision changes.
The browser renders only fixed counter fields and bounded safe labels, never
connection URLs, configuration blobs or arbitrary component reasons.

The export worker and its operational management integration are separate backlog
items (#99–#105). Until an owning worker reports these fields, they remain
unavailable rather than being synthesized from readiness or queue size. Per-station
reconnects and in-flight calls are unavailable in this aggregate health source.
The panel shows up to ten station identities, protocols and last captured heartbeat
timestamps from existing authorized OCPP traces, respecting the paused window.
Missing heartbeat traces are not interpreted as a disconnected charger. No global
station inventory is added to the public health endpoint.

Tests use the real management router with explicit passive exporter-outage
observations, and the actual daemon for disabled-export behavior. These checks
exercise the panel and reporting boundary, not a PostgreSQL provider or charging
acceptance matrix.
