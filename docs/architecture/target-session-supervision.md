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

The session reports starting and terminal/retry-classified health without changing core readiness.
A target outage therefore does not stop station actors, local authorization, storage, or authorized
management access.

The supervisor can reconstruct a fresh adapter from the same validated selection. Pending work
remains host-owned and can be re-enqueued in durable recovery order; another target revision is
rejected instead of receiving it. Durable outbox reads, outcome persistence, retry backoff, expiry,
and reconciliation are intentionally owned by the follow-on delivery-policy worker rather than by
an adapter-private retry queue.

Graceful shutdown signals the target and waits only for the caller-supplied duration. A task that
misses that bound is aborted and reported as a shutdown deadline failure. Dropping the supervisor
also aborts the owned target task, so no detached target session survives its composition owner.
