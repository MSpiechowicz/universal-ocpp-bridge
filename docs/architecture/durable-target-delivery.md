# Durable target delivery

Issue #35 adds host-owned scheduling for target-neutral outbox records. Charging operations commit
their state, journal events, and required target deliveries atomically, then return without waiting
for MQTT, EMS/SCADA, or another external target. A separate bounded worker reads only the selected
target instance and immutable configuration revision, so an unavailable target cannot turn local
charging into an external-service dependency.

The worker admits ready entries to the target session in stable insertion order. Only the oldest
pending entry for each canonical resource key is eligible, while unrelated stations and resources
can progress concurrently. Critical and replaceable-latest-state classes have independent
completion semantics and exponential backoff bounds. Queue saturation or a stopped target leaves
the durable row untouched for a later bounded poll.

Every adapter report retains its exact meaning. Local API or SSE exposure is recorded as local
exposure and does not satisfy a policy that requires named-peer acknowledgement. Peer identity and
acknowledgement scope remain explicit. Retryable or semantically insufficient outcomes persist a
next-attempt instant and attempt count; permanent outcomes, matching completion outcomes, and
deadline classification preserve an audit record before removing the outbox row.

Delivery IDs, event IDs, target instance IDs, and configuration revisions remain unchanged across
attempts and process restarts. If a peer consumed a message but the process crashed before the
report transaction committed, recovery reads the same pending row and may resend the same stable
ID. This is intentionally at-least-once behavior for acknowledged delivery, never exactly-once.
Target implementations and their peers must use the stable ID for deduplication where their
declared policy requires it.
