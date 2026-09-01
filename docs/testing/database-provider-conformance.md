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
registration without changing OCPP handlers or target adapters. The future PostgreSQL provider and
its scheduler must reuse these host and outcome checks rather than copy a weaker happy-path test.
