# Operational storage retention and start admission

Issue #39 adds one application-owned pressure decision shared by every command ingress. The
authoritative SQLite adapter defaults to seven days of operational history, a 256 MiB logical
journal/outbox budget, and a 16 MiB reserve that routine work and new charging sessions cannot
consume. Deployments may choose smaller validated limits for constrained tests, but the reserve
must be strictly smaller than the budget.

`AtomicStoreWrite` classifies capacity use as routine work, a new-session start, or active-session
completion. The common command coordinator marks every canonical `Start` before it reaches an
outward management, target, or provider path. When the non-reserved budget cannot contain the
start, storage returns `CapacityExhausted`, which becomes the stable
`StorageCapacityExhausted` command-admission reason. Stop lifecycle and result persistence are
classified as active-session completion and may use the protected reserve. Routine journal work
cannot consume that reserve.

The budget uses deterministic logical encoded sizes instead of SQLite file length. Database pages,
WAL checkpoint timing, and free-page reuse therefore cannot make admission oscillate. Journal
events, target deliveries, committed operational records, and delivery audit history contribute to
reported use. Physical disk-full handling remains a separate host/storage failure and cannot be
made safe by logical accounting alone.

## Retention and pressure order

The adapter applies pressure in this order inside the same immediate transaction as the owning
write:

1. Remove already-retained best-effort telemetry and best-effort deliveries before refusing a
   critical write.
2. Drop incoming best-effort records that do not fit, while persisting cumulative dropped counts
   by category.
3. Refuse a new start or routine journal write before it enters the active-session reserve.
4. Allow explicitly classified active-session completion work up to the full budget.

No pressure path deletes a critical pending delivery. Seven-day maintenance removes an expired
journal event only when no target delivery for that event remains. It likewise removes expired
delivery-attempt history only after the delivery is final. Rows migrated from older schemas have no
invented retention deadline and remain retained until an independently safe policy can classify
them.

`StorageRetentionStatus` reports current logical use, the common new-session admission state,
retained critical events and required deliveries, shed best-effort categories, and safely pruned
history. Because status is recalculated from committed rows, completing a delivery and running
maintenance immediately restores admission when use falls below the non-reserved limit. This is a
bounded outage policy, not a promise to preserve an unlimited required backlog.

Schema version 4 adds explicit retention deadlines and durable category counters. It does not
rewrite or assign synthetic deadlines to pre-version-4 rows.
