# Database provider conformance

`uob-database-conformance` is a reusable, hardware-free fake-host harness for optional external
database adapters. Concrete provider tests pass their real `DatabaseProvider` a bounded
`DatabaseExportContext`; the retained host driver supplies privacy-filtered canonical batches and
observes validated critical reports, best-effort diagnostics, backpressure, and graceful shutdown.
The harness has no charger-command, selected-target, socket, arbitrary SQL, or operational-store
surface.

Every provider test suite must inspect its descriptor and exercise all advertised schema versions
and record classes. It must also cover stable-ID retries, destination configuration revision
isolation, batching, confirmed, retryable, permanent, uncertain, and partial outcomes when the
provider advertises per-record transactions. Transport handoff is never commit evidence. Tests
must prove that queue limits are enforced, invalid reports are rejected, repeated identities are
deduplicated without overwrite, conflicting content is an integrity failure, unsupported records
fail explicitly, shutdown is bounded, diagnostics are sanitized, and offline factory validation
does not resolve credentials, DNS, or make a connection.

Provider acceptance is run under each supported host selection: exporter disabled, MQTT, direct
HTTP EMS/SCADA, and the EMS/SCADA MQTT preset. Because export is independent of the selected target,
these modes change composition only and use the same provider scenarios. Hardware-free integration
adds injected connection outage, commit-before-acknowledgement loss, slow reports, full batch and
diagnostic queues, stale configuration revisions, and resource-ceiling cases. A provider that is
silent for unsupported work or exposes credential text fails the suite rather than timing out the
charging path.

The reference replacement provider in
[`provider_behavior.rs`](../../tests/database-conformance/tests/provider_behavior.rs) demonstrates
the reusable contract. Registry coverage in
[`provider_registry.rs`](../../adapters/export/tests/provider_registry.rs) demonstrates that a
second provider is added through its adapter factory, credential-free schema, and composition-root
registration without changing OCPP handlers or target adapters. The private PostgreSQL
[client qualification](postgresql-client-qualification.md) exercises TLS/SCRAM, fault and
resource behavior against a disposable database, but it does not instantiate a `DatabaseProvider`.
PostgreSQL remains unavailable to the service; `data_export.enabled = true` is rejected.
The offline single-worker scheduler accepts an injected atomic provider and independently
durable spool, but does not make production PostgreSQL delivery available. #104 must implement
the concrete provider's per-connect/per-query deadlines and at-most-two-connections contract:
the opaque host can bound only total attempt/shutdown time. #101 partial-commit, poison-record
and reconciliation behavior remains pending.

## Offline scheduler and spool regression scenarios (#100)

From the repository root on Linux, the focused commands below exercise the existing
hardware-free suites (the service/spool fixture needs a distinct `/dev/shm` device):

```sh
cargo test -p uob-storage-adapter --test export_spool delivery:: -- --nocapture
cargo test -p uob-external-export-adapter --test export_scheduler -- --nocapture
cargo test -p uob-service --lib export_runtime_service_tests::scenarios:: -- --nocapture
```

The spool suite covers schema v2→v3 preservation, a durable ordered claim
replayed after reopen with its original batch ID, conflicting/stale reports,
atomic whole-batch confirmation and a durable `last_confirmed_batch` and
`confirmed_records` count **separate** from v12 source checkpoints. It also
covers claimed telemetry protected against pressure and an oversized pending
critical record never falsely confirmed. Claim admission conservatively
reserves for escaped fields plus overhead before the scheduler's independent
exact encoded 256 KiB limit; a claim can backpressure even when its actual
encoding might fit.

The scheduler suite exercises disabled selection (no worker), ready-path
dispatch within 1 s in the low-load fake-clock scenario, serial retries of
the same claim with jittered backoff, aborted-session replay after reopen,
one-at-a-time hung-provider attempts and bounded shutdown. Its per-attempt
total deadline is 5 s and provider-task shutdown grace is 2 s; an opaque
provider's connect/query phases and connection count are not independently
enforced by this suite. The service scenarios exercise a real local OCPP
session before and during injected provider failure/hang, independent
readiness and bounded drain, rejection of production enabled export and
missing-spool startup, and disabled startup without an exporter. No command
above tests a live production PostgreSQL export.
