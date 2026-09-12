# Production probation

A successful activation leaves the journal-owned candidate in `probation`. The trusted host
collector calls `Supervisor::observe_probation` with measurements for that production artifact,
configuration and process invocation. The release IPC has no operation for submitting measurements
or declaring a release healthy. Without a collector, the candidate remains in probation.

The administrator selects a production resource/health profile, identifies its exact thresholds
and collector configuration by SHA-256 digest, and supplies a policy requiring at least 86,400
seconds (at most 30 days), with a maximum measurement interval of 1–300 seconds. Changing that
policy during probation is rejected. The collector must evaluate all six required checks:

- Actual core-worker progress.
- Actual storage-worker progress.
- Core readiness.
- Process memory within the deployment budget.
- Process CPU within the deployment budget.
- Local response latency within the deployment budget.

These are trusted evaluated observations, not values accepted from a browser, charger, or release
client. Missing measurements are not successful checks. The host binds each observation to the
running artifact and configuration, supplies a fresh process-invocation digest, a strictly increasing
persistent observation ID, UTC seconds, and monotonic process uptime. External target outages do
not themselves mean that the core checks failed; rollback classification remains a separate policy.

The first observation durably records the probation evidence start and earns no time. Only intervals
between passing observations for the same process count, capped by the smaller UTC/uptime delta.
Missing/failed checks, excessive intervals, forward clock jumps and process changes reset accumulated
healthy time. Backward UTC and repeated/reordered IDs are rejected. A supervisor restart preserves
committed progress but the first new observation earns no elapsed time: downtime is never credited.
A process/host restart changes invocation and resets the healthy run. This can extend probation past
24 hours; the original evidence start remains visible along with interrupted-interval count.

Evidence is bounded and atomically persisted in the private supervisor ledger before the activation
journal can mark the candidate `healthy`. Failed or uncertain writes block advancement. A restart
between the evidence commit and healthy transition requires another fresh passing observation;
healthy journal recovery is already idempotent. Read-authorized status includes the policy,
start, verified seconds, interruptions and latest checks. Later health failures belong to the
persistent failure classifier rather than restarting probation on an already healthy release.

Probation completion leaves `previous-good` unchanged and retained. Only a later qualified
promotion may replace that fallback with the now-healthy active artifact. Neither measurements nor
probation completion restore a database, replace configuration, or control chargers.

Verification uses deterministic trusted-host observations over the full 24-hour interval, actual
private ledger persistence and signed artifact/journal operations. It covers every missing/failed
check, stale identities and IDs, changed policy, clock jumps, supervisor/process restart, interrupted
writes, evidence-before-journal recovery, and fallback retention. These are policy tests, not a
24-hour physical Pi resource measurement.

```text
cargo test --locked --package uob-release-manager --lib probation
```
