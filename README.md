# universal-ocpp-bridge

Universal OCPP bridge for the external protocols

See [the architecture and implementation plan](.agents/PLAN.md) for the agreed scope,
Rust service and simulator architecture, selectable MQTT and EMS/SCADA targets
(direct API or optional MQTT broker), extensible target/data contracts, connection
protocols, external database export, browser debugging, staging, automatic
rollback, and CI/release strategy.

Implementation has started with the buildable workspace foundation. Charging
behavior, simulator scenarios, and production release supervision remain planned.

## Workspace foundation

The initial Rust modular-monolith workspace is organized into protected
contracts/domain/application crates, outward-facing adapter crates, and separate
service, simulator, and release-manager executables. See
[ADR 0001](docs/architecture/0001-modular-monolith-boundaries.md) for the package
rules and first-release exclusions.

The canonical HTTP, MQTT, and export JSON contracts are published as versioned Draft 2020-12
schemas under `crates/contracts/schemas`. See
[`docs/contracts/json-schema-versioning.md`](docs/contracts/json-schema-versioning.md) for the v1
compatibility policy and regeneration command.

Run the current foundation checks with:

```text
./scripts/verify-workspace.sh
```

The command uses a local Cargo toolchain when Cargo, rustfmt, and Clippy are
available. Otherwise it automatically runs the checks in the pinned
`rust:1.98.0-bookworm` Docker image. Install Rust with rustup or ensure Docker is
installed and running.

Contributions use pinned, Rust-native Conventional Commit and pull request title checks. See the
[contribution policy](docs/contributing.md) for the accepted format, Cocogitto version, adoption
baseline, feature-squash policy, automatic semantic-version releases, and local verification
commands.

The service has a noninteractive `uob` CLI for offline configuration validation, headless startup,
optional static-asset disabling, and authenticated JSONL event consumption. See the
[headless CLI guide](docs/operations/headless-cli.md) for configuration, stream security, and exit
codes.

Runtime bridge, environment, release, process, and selected-target identity is owned by the
service at startup. See [runtime identity configuration](docs/configuration/runtime-identity.md)
for production defaults and isolated staging/demo examples.

Optional external database export is selected independently of the bridge target and remains bound
to one stable destination revision. See [external export configuration](docs/configuration/external-export.md)
for disabled behavior, PostgreSQL settings, TLS requirements, and safe destination changes.

Diagnostic observations are centrally redacted and serialized before any downstream sink can see
them. See [the diagnostic redaction boundary](docs/security/diagnostic-redaction.md) for the typed
safe-field policy, fail-closed vendor payload handling, and inert-renderer requirement.

Optional diagnostic hooks preserve correlation across socket, application, commit, command and
target boundaries. See [correlated diagnostic instrumentation](docs/architecture/correlated-diagnostics.md)
for exact evidence semantics, bounded state changes and process-local timing.

Diagnostic capture requires explicit enablement and scoped read/capture permissions. See
[diagnostic capture controls](docs/security/diagnostic-capture.md) for session deadlines and permissions.
The [shared bounded trace ring and authenticated SSE](docs/architecture/bounded-debug-traces.md)
retain at most 8 MiB / 2,000 records, expose explicit best-effort gaps, and release slow subscribers
without blocking producers. [Bounded capture-file export](docs/security/diagnostic-capture-export.md)
streams a finite redacted window with provenance, gaps and independently enforced download limits.

Normal release promotion is fail-closed on real old-to-new-to-old data evidence. See the
[reversible release compatibility policy](docs/operations/release-compatibility.md) for schema
ranges, additive migration rules, configuration projections, and security floors.

Dependency, secret, workflow, and source-SBOM checks are fail-closed and use pinned tools. See the
[dependency and workflow security policy](docs/security/dependency-and-workflow-policy.md) for the
review rules, fixture evidence, and current source-versus-package SBOM boundary.

Stable source publication is gated by live branch, merge, Actions, and protected-environment
settings rather than workflow comments alone. See
[repository release protection](docs/operations/repository-release-protection.md) for the required
GitHub configuration and fail-closed verification command.

OCPP release coverage is tracked separately from implementation claims. See the
[independent OCPP fixture corpus](docs/testing/ocpp-fixture-corpus.md) for pinned specification
provenance, hand-authored wire fixtures, the coverage-to-test matrix, and its fail-closed release
gate.

The pinned OCPP model crate is isolated behind separate 1.6J and 2.0.1 adapters. See the
[OCPP model adapter boundary](docs/architecture/ocpp-model-adapters.md) for supported negotiation,
validation, application mappings, explicit gaps, and non-goals.

Authenticated charger sockets terminate at the bounded Axum OCPP endpoint before entering station
state. See the [OCPP WebSocket endpoint](docs/architecture/ocpp-websocket-endpoint.md) for routes,
subprotocol negotiation, credential and mTLS admission, duplicate handling, and transport limits.

Bidirectional OCPP calls use a bounded socket-owning lifecycle with correlated replies, explicit
timeouts and conservative uncertain-transmission outcomes. See
[OCPP call lifecycle](docs/architecture/ocpp-call-lifecycle.md) for validation, duplicate and late
response behavior, application response control, and hostile-peer evidence.

OCPP 1.6 remote start, stop, reset and unlock use the authorized durable command path with
separate protocol responses and observed effects. See [OCPP 1.6 remote control](docs/architecture/ocpp16-remote-control.md)
for native validation, local identity resolution, deadlines and recovery.

OCPP 1.6 availability commands retain scheduled intent separately from committed connector and
station observations. See [OCPP 1.6 availability](docs/architecture/ocpp16-availability.md) for
scoped control, transaction completion, durable evidence, and reconnect behavior.

OCPP 2.0.1 remote commands preserve EVSE targeting, durable remote-start correlation and native
scheduled-reset evidence. See [OCPP 2.0.1 remote control](docs/architecture/ocpp201-remote-control.md)
for scoped dispatch, response/effect separation and restart behavior.

Native report workflows can use [bounded multipart collection](docs/architecture/multipart-reports.md)
for correlated ordered fragments, shared memory admission, cancellation, and absolute deadlines.

Future industrial drivers remain behind the target registry and canonical data/command ports. See
the [industrial adapter extension boundary](docs/architecture/industrial-adapter-extension.md) for
the mapping checklist, unavailable first-release OPC UA kind, and compatibility limits.

Critical target deliveries remain owned by their original target instance and configuration
revision across restarts. See [target destination changes](docs/configuration/target-destination-changes.md)
for offline previews, restart-required state, audited archive/discard handling, and dispatch guards.

The selected adapter runs in a bounded host-owned session with guarded command ingress, isolated
critical reporting, and deadline-enforced shutdown. See
[target session supervision](docs/architecture/target-session-supervision.md) for lifecycle,
recovery, and durable-delivery boundaries.

Required target deliveries are scheduled from the target-neutral durable outbox without blocking
local charging on target availability. See
[durable target delivery](docs/architecture/durable-target-delivery.md) for ordering, retry,
acknowledgement, recovery, and at-least-once semantics.

Command retries are deduplicated atomically across concurrent submissions and restarts, with safe
conflicts, known-result replay, and protected unresolved outcomes. See
[durable command deduplication](docs/architecture/command-deduplication.md) for fingerprint and
seven-day retention semantics.

Commands for offline stations are rejected before durable queue admission, while in-flight
commands with ambiguous transmission remain unresolved and are never replayed after restart. See
[uncertain command recovery](docs/architecture/uncertain-command-recovery.md) for live-session
dispatch classification and observed-state reconciliation.

Critical business-event consumers resume from resource-scoped durable checkpoints, including at
the current live end. See [durable event cursors](docs/architecture/durable-event-cursors.md) for
restart behavior, expired-cursor snapshot recovery, and separation from telemetry and traces.

Operational history uses a bounded seven-day journal/outbox policy with capacity protected for
active-session completion. See
[storage retention and start admission](docs/architecture/storage-retention-admission.md) for
pressure ordering, safe pending-delivery retention, shared start refusal, and recovery counters.

OCPP station admission defaults to a preconfigured identity allowlist, TLS, and a unique
high-entropy credential per station, with certificate-bound mutual TLS available as the stronger
mode. See [station transport authentication](docs/security/station-transport-authentication.md) for
offline validation, secret resolution, handshake ordering, safe failures, and WSS test evidence.

Management and direct EMS/SCADA listeners default to loopback, while any remote listener requires
explicit enablement, TLS, and resource-scoped credentials. See
[management and integration access policy](docs/security/management-and-integration-access.md) for
the shared read/control/privileged command guard and equivalent MQTT ACL classes.

Charging identities are resolved to opaque SHA-256 references and decided from a persisted local
allowlist, so target and internet outages do not disable authorized charging. See
[local authorization policy](docs/security/local-authorization.md) for expiry, revocation, resource
scope, restart recovery, command-ingress enforcement, and production test-provider guards.

OCPP 2.0.1 charger authorization preserves typed identity and certificate evidence, applies current
durable local policy after bounded provider resolution, and retains native status/expiry semantics.
See [OCPP 2.0.1 authorization](docs/security/ocpp201-authorization.md) for provider boundaries and tests.

The standalone simulator has a versioned deterministic TOML scenario contract and a machine-readable
JSONL runner. See the [scenario runner guide](docs/simulator/scenario-runner.md) for its actions,
failure categories, timeout model, and checked-in example.

The simulator's OCPP 1.6 charging example exercises registration, authorization, status,
transaction start/meter/stop, active-transaction reconnect, exact wire fixtures, and separate
remote-command acceptance without importing bridge state-machine code.

The OCPP 2.0.1 charging example exercises the corresponding native multi-EVSE flow with
`TransactionEvent` sequencing, complete meter-quality fields, reconnect continuity, exact
independent fixtures, and separate RequestStart/RequestStop acceptance.

Core readiness, new-session admission, component degradation, and resource counters are exposed
separately. See [health and metrics](docs/operations/health-readiness-metrics.md) for endpoint and
failure semantics.

The management adapter can expose canonical, resource-scoped station inventory and snapshot reads
through bounded application query ports. See the
[management read API](docs/operations/management-read-api.md) for routes, limits, and failure
semantics.

The direct EMS/SCADA listener publishes a versioned OpenAPI contract with same-listener canonical
schema references and an offline CI drift gate. See the
[EMS/SCADA OpenAPI contract](docs/contracts/ems-scada-openapi.md) for regeneration and the
broker-free Rust contract demo.

See [service packaging and shutdown](docs/operations/service-lifecycle.md) for the non-root
systemd unit, bounded journal namespace, shutdown deadlines, and SQLite drain/recovery contract.

See [production and staging filesystem isolation](docs/operations/environment-filesystem-isolation.md)
for separate service accounts, units/slices, configuration, databases, runtime locks and journals,
with same-Pi and separate-Linux-host staging layouts.

See [staging network isolation](docs/operations/environment-network-isolation.md) for the mandatory
loopback-only test namespace, production-socket denial, and fail-closed staging configuration.

See [staging resource admission and shedding](docs/operations/staging-resource-governor.md) for
cgroup limits, production health alarms, and whole-slice shutdown under pressure.

Qualified artifacts can cross the live idle boundary through the
[production activation coordinator](docs/operations/production-artifact-activation.md), with confirmed
process shutdown, durable interruption recovery, and production state retained in place.

See [service readiness and watchdog](docs/operations/service-watchdog.md) for worker-backed
progress, bounded restart policy, and per-invocation termination evidence.

See [disk budgets and installation admission](docs/operations/staging-disk-preflight.md) for
fixed partition isolation, allocated installation capacity, and retained-artifact protection.

See [signed application artifact installation](docs/operations/signed-artifact-store.md) for
trusted manifests, bounded extraction, immutable candidates, and protected fallback retention.

See [independent release supervisor IPC](docs/operations/release-supervisor-ipc.md) for
kernel-authenticated local permissions, private request/failure state, independent packaging,
and fail-closed activation boundaries while the bridge is stopped.

See [durable release transitions and ownership](docs/operations/release-activation-journal.md)
for activation intent recovery, atomic artifact pointers, candidate retention, and
the boundary between persistent observations and production process control.

See [trusted candidate qualification](docs/operations/release-qualification.md) for
signed acceptance/compatibility evidence, exact staging input bindings, the 24-hour
soak requirement, and separate promotion authorization.

See [production release preflight and backup](docs/operations/release-preflight-backup.md)
for candidate configuration checks, bounded online SQLite backups, and disaster-recovery ownership.

See [release drain and the idle boundary](docs/operations/release-drain.md) for shared start
admission, durable work inventory, deadline deferral, and staging-stop ordering.

See [production probation](docs/operations/production-probation.md) for the durable 24-hour
health/resource gate, missing-evidence behavior, restart accounting and fallback retention.

See [persistent release failure classification](docs/operations/rollback-signal-policy.md)
for reboot-safe trigger windows, external-outage exclusions, staging pressure ordering,
and critical recovery decisions.

See [OCPP 1.6 registration and status](docs/architecture/ocpp16-registration.md) for persisted boot
decisions, heartbeat gating, native connector status, and reconnect behavior.

See [OCPP 2.0.1 registration and status](docs/architecture/ocpp201-registration.md) for durable boot
policy decisions, heartbeat gating, native EVSE/connector status, and reconnect recovery.

See [OCPP 1.6 durable transactions](docs/architecture/ocpp-16-transactions.md) for committed
start/stop replies, CSMS IDs, local authorization, meter evidence and bounded retry recovery.

Eligible release failures have a durable one-attempt fallback path that preserves current data.
See [automatic rollback](docs/operations/automatic-rollback.md) for host integration, quarantine
and operator-recovery behavior.
