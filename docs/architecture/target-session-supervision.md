# Target session supervision

The target adapter package starts exactly the factory instance selected by the validated registry
configuration. Validation and construction remain network-free; the adapter opens its protocol
connections only inside its one supervised `BridgeTarget::run` future.

## Host-owned boundaries

Each session receives a bounded FIFO delivery receiver, scoped canonical queries, guarded command
admission, a capacity-reserved critical report port, nonblocking best-effort diagnostics, and an
explicit shutdown signal/deadline. Delivery ingress accepts only the selected target instance and
immutable configuration revision. It reserves the shared target-egress budget before enqueueing
and returns the original work when the queue is full, closed, mismatched, or over budget.

The command wrapper accepts only target-authenticated origins for this exact instance, checks the
operation against the descriptor, bounds encoded payload size, and rejects work immediately when
the target command allowance is occupied. The application command port still owns authorization,
safety, expiry, durable admission, idempotency, dispatch, and result persistence. This keeps target
commands on the same path as management commands and preserves their return route.

Critical delivery reports use their own semaphore and the process critical-report budget before
reaching host durability policy. A full or disabled diagnostic sink cannot consume that reserved
capacity. Adapters must keep protocol readers and keepalives independent of slow delivery/report
futures, as required by the reusable target conformance suite.

## Restart and shutdown

The service starts one selected target from the validated registry only when the demo charging
runtime is present. It binds the canonical SQLite charging store to scoped station reads and
retained events, routes target commands through the same durable command coordinator and
authorization service as management, and projects target health into the common health monitor.
The MQTT command principal is bound to the configured target and station roster; HTTP credentials
are resolved by the HTTP adapter's own credential parser into host command grants. Without an
HTTP credential file, the host creates only a read-only command guard.

Charging transaction events are committed alongside their required outbox entries. The selected
target's bounded delivery worker resolves each entry against its exact retained journal event and
records adapter outcomes in the same SQLite worker. A bounded station inventory scan also sends
replaceable snapshots to the selected target and refreshes them periodically; these state
publications are not durable transaction-event acknowledgements.

Unexpected termination of the selected session or its delivery worker fails service supervision;
a stop signal shuts down the session, then its delivery worker, before the charging store. The
host enforces the lifecycle shutdown deadline even if a target ignores its own shutdown signal.
