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

Runtime bridge, environment, release, process, and selected-target identity is owned by the
service at startup. See [runtime identity configuration](docs/configuration/runtime-identity.md)
for production defaults and isolated staging/demo examples.

Optional external database export is selected independently of the bridge target and remains bound
to one stable destination revision. See [external export configuration](docs/configuration/external-export.md)
for disabled behavior, PostgreSQL settings, TLS requirements, and safe destination changes.

Diagnostic observations are centrally redacted and serialized before any downstream sink can see
them. See [the diagnostic redaction boundary](docs/security/diagnostic-redaction.md) for the typed
safe-field policy, fail-closed vendor payload handling, and inert-renderer requirement.

Normal release promotion is fail-closed on real old-to-new-to-old data evidence. See the
[reversible release compatibility policy](docs/operations/release-compatibility.md) for schema
ranges, additive migration rules, configuration projections, and security floors.

Dependency, secret, workflow, and source-SBOM checks are fail-closed and use pinned tools. See the
[dependency and workflow security policy](docs/security/dependency-and-workflow-policy.md) for the
review rules, fixture evidence, and current source-versus-package SBOM boundary.

OCPP release coverage is tracked separately from implementation claims. See the
[independent OCPP fixture corpus](docs/testing/ocpp-fixture-corpus.md) for pinned specification
provenance, hand-authored wire fixtures, the coverage-to-test matrix, and its fail-closed release
gate.

The pinned OCPP model crate is isolated behind separate 1.6J and 2.0.1 adapters. See the
[OCPP model adapter boundary](docs/architecture/ocpp-model-adapters.md) for supported negotiation,
validation, application mappings, explicit gaps, and non-goals.

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

The standalone simulator has a versioned deterministic TOML scenario contract and a machine-readable
JSONL runner. See the [scenario runner guide](docs/simulator/scenario-runner.md) for its actions,
failure categories, timeout model, and checked-in example.
