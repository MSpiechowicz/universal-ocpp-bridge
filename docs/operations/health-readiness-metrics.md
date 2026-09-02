# Health, readiness, and resource metrics

The management listener exposes `GET /health` and `GET /api/v1/health` as the same structured
health snapshot. Both return HTTP 200 only while the local core loop is responsive and
authoritative storage can safely continue existing charging work. Startup, core-loop failure, or
storage failure returns HTTP 503.

`readiness` and `accepts_new_sessions` are separate. Storage capacity protection keeps the core
ready for active-session completion while setting `accepts_new_sessions` to false. The storage
projection includes the logical budget, protected reserve, current use, pending required
deliveries, retention progress, and intentionally shed best-effort records.

Target, MQTT broker, EMS client, and optional external-export failures are component states. They
report reconnect counts, bounded backlog, in-flight work, connection count, and a sanitized reason,
but cannot by themselves make the charging core unready. Target retry and exporter queues still
consume and appear in the daemon's shared runtime budget.

`GET /metrics` returns a text exposition of the same snapshot. It includes:

- local-response and storage latency count, fixed-bucket p95 upper bound, maximum, and overflow;
- queue item and aggregate payload pressure, reconnects, dropped telemetry/diagnostics, storage
  use, pending required deliveries, and uncertain commands;
- daemon RSS and cumulative CPU time sampled from Linux `/proc` when available; and
- separately labeled measurements supplied for the broker, external database, browser, simulator,
  providers, and release manager.

The daemon resource budget includes its selected target adapter and optional exporter. Auxiliary
process measurements are never added to daemon RSS or CPU. The counters provide evidence inputs
for the plan's 128 MiB RSS, 10% of one Pi 4 core, and 100 ms p95 local-response targets; exposing a
counter is not a performance or Raspberry Pi qualification claim.
