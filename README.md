# universal-ocpp-bridge

Universal OCPP bridge for the external protocols

See [the architecture and implementation plan](.agents/PLAN.md) for the agreed scope,
Rust service and simulator architecture, selectable MQTT and EMS/SCADA targets
(direct API or optional MQTT broker), extensible target/data contracts, connection
protocols, external database export, browser debugging, staging, automatic
rollback, and CI/release strategy.

The workspace includes an opt-in demo-only loopback charging ingress and scoped station
views. The packaged production service remains management/storage-only: installing it
does not establish production charging, a connected supervisor activation host, or live
rollback.

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

The [product version proposal policy](docs/operations/product-version-policy.md) defines explicit
commit bump rules, read-only JSON proposals, and the separate first-stable readiness decision.

The [candidate channel policy](docs/operations/candidate-channel-versioning.md) defines read-only
rc numbering and same-artifact stable proposals with ancestry and tree checks.

The service has a noninteractive `uob` CLI for offline configuration validation, headless startup,
optional static-asset disabling, authenticated JSONL event consumption, and independent
supervisor-backed release commands. See the [headless CLI guide](docs/operations/headless-cli.md)
for configuration, release permissions and policy errors, stream security, and exit codes.

The optional [browser console](docs/operations/browser-console.md) embeds compiled static assets
with no Node runtime on the Raspberry Pi. It verifies destination identity, keeps scoped
credentials in tab memory, binds diagnostic tokens to their environment, requires explicit destination
confirmation for controls, and shows bounded authenticated SSE connection and stale-state evidence.
The [demo charging station configuration](docs/operations/headless-cli.md#demo-charging-station-views)
connects authenticated OCPP 1.6J and 2.0.1 peers to durable station, EVSE, connector,
transaction and measurement views. Separately provisioned read, control and privileged grants
plus per-station action opt-ins permit the browser's supported start, stop, charging-limit and
schema-validated privileged commands. Request admission, charger response and later linked
observed effects remain distinct; acceptance does not prove physical charging. The opt-in
loopback plaintext listener and these local grants are **demo-only**, not production charging
ingress or command exposure.

The [protected OCPP 1.6 local-list/cache commands](docs/operations/headless-cli.md#protected-ocpp-16-station-authorization-list-and-cache)
use independent default-off privileged options and reference-only provisioning.
Native acknowledgements remain distinct from installed contents and offline use.
The [independent simulator](docs/simulator/scenario-runner.md#native-ocpp-16-local-list-cache-and-real-offline-recovery)
provides bounded owner-only persistence, genuine socket-offline authorization,
transaction replay, actual native Reset and new-process recovery without changing
the service's separate SHA allowlist or enabling production controls.

The [Debug operational panels](docs/operations/debug-operational-panels.md) separate local persistence,
resource pressure and remote export evidence with explicit unavailable observations.

The [simulator evidence panel](docs/operations/debug-simulator-evidence.md) reads isolated
demo/staging run outcomes and links supplied correlations into the existing Debug timeline.

The [browser and API diagnostics panel](docs/operations/browser-diagnostics.md) exposes bounded,
sanitized failure counters and correlation lookup alongside stream staleness and reconnect evidence.

The [offline capture inspector](docs/operations/offline-capture-inspector.md) opens bounded JSONL
exports locally with file provenance, gaps and inert inspection, without a live API connection.

The [bounded Debug timeline](docs/operations/debug-timeline.md) adds explicit scoped capture controls,
virtualized read-only traces, retained-window filters/bookmarks and visible gap/eviction evidence.

Runtime bridge, environment, release, process, and selected-target identity is owned by the
service at startup. See [runtime identity configuration](docs/configuration/runtime-identity.md)
for production defaults and isolated staging/demo examples.

Optional external database export is selected independently of the bridge target and remains bound
to one stable destination revision. See [external export configuration](docs/configuration/external-export.md)
for disabled behavior, PostgreSQL settings, TLS requirements, and safe destination changes.
An [explicit PostgreSQL provisioning command](docs/configuration/external-export.md#explicit-postgresql-schema-provisioning)
installs the canonical destination schema and least-privilege grants independently of the runtime driver.

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

OCPP 1.6J and 2.0.1 DataTransfer use edition-specific vendor/message registration, bounded
opaque data, and persisted outcomes in both directions. See the [1.6J boundary](docs/architecture/ocpp-model-adapters.md#ocpp-16j-datatransfer)
and [2.0.1 boundary](docs/architecture/ocpp-model-adapters.md#ocpp-201-datatransfer)
for provider deadlines, default redaction, embedding responsibilities, and no-replay recovery.

OCPP 1.6 remote start, stop, reset and unlock use the authorized durable command path with
separate protocol responses and observed effects. See [OCPP 1.6 remote control](docs/architecture/ocpp16-remote-control.md)
for native validation, local identity resolution, deadlines and recovery.

OCPP 1.6J `TriggerMessage` is an opt-in, privileged demo command for six native
message classes. Its native reply is separate from later compatible station
observations; neither acceptance nor a matching observation proves causation or
physical charging. See [OCPP 1.6 remote control](docs/architecture/ocpp16-remote-control.md)
for connector scope, the 60-second window and one-shot recovery.

OCPP 1.6J `GetCompositeSchedule` is a default-off, privileged demo query for grid
aggregation or an exact configured connector. Its durable typed result preserves
exact rates, optional native metadata and genuine zero separately from Rejected;
restart/reconnect never automatically replays the query. It does not calculate
or enforce a local schedule or install profiles.
See [OCPP 1.6 remote control](docs/architecture/ocpp16-remote-control.md#opt-in-getcompositeschedule)
for opt-in, strict native validation and independently observed evidence.

OCPP 2.0.1 `GetCompositeSchedule` (K08) and `GetChargingProfiles` (K09) are separately
default-off, privileged demo queries for the grid connection or an exact configured EVSE.
Composite results keep exact limits, phases and standardized reasons. Installed-profile
queries record the native acknowledgement first, then collect `ReportChargingProfiles`
fragments within shared bounds into typed profiles with their EVSE and limit source;
restart interrupts a pending report and nothing is replayed. Reported profiles never
become local policy. See [OCPP 2.0.1 remote control](docs/architecture/ocpp201-remote-control.md#opt-in-composite-schedules-and-installed-profile-reports).

OCPP 2.0.1 charging needs, EV charging schedules and external charging limits (K11–K17) are
recorded and answered within the bridge's own authority. Charging needs get `Rejected` with
`NotEnabled` by default, or `Processing` when an operator's EMS sends the `TxProfile` through
this bridge. EV schedules are checked exactly against the bridge's own installed TxProfiles.
External limits and their releases become observed snapshot state and typed journal events.
CSO-sourced limits are refused, and nothing is calculated, enforced or turned into a command.
See [OCPP 2.0.1 charging needs and external limits](docs/architecture/ocpp201-charging-negotiation.md).

Full native OCPP 2.0.1 SetChargingProfile/ClearChargingProfile is independently default-off,
privileged and demo-only, with exact station/EVSE scope, native schedule/transaction/phase
evidence and no automatic replay. Enabling full Set also blocks canonical charging limits
until the operator intentionally performs three all-EVSE purpose-only Clears; those clears
can remove existing station policies and assume exclusive CSMS ownership. A bounded durable
ownership ledger protects conflicts, uncertainty, station-global replacement authority and
active profiles from history pruning. Canonical-only mode remains usable without that
baseline. That addition introduced result v1.7/embedded export v1.8 while preserving
historical schema bytes; SQLite13→14 is additive, but13 binaries reject14 and downgrade is
not qualified. No hardware enforcement, certification, privileged target ingress, complex
browser editor or global native-command export delivery is claimed. See
[native201 charging profiles](docs/architecture/ocpp201-remote-control.md#opt-in-native-charging-profiles)
and [operator prerequisites](docs/operations/headless-cli.md#full-native-ocpp-201-setclear-charging-profiles).

OCPP 2.0.1 `TriggerMessage` is separately opt-in for eleven native message
classes, with station, EVSE and connector targeting. Its privileged demo command
retains the native reply independently of a 60-second window of compatible,
non-causal observations; accepted replies do not prove later reports, certificate
signing or physical charging. See [OCPP 2.0.1 remote control](docs/architecture/ocpp201-remote-control.md#opt-in-triggermessage)
for scope, durable evidence and no-replay recovery.

OCPP 2.0.1 read-only `GetVariables`, `GetBaseReport` and `GetReport` are
individually default-off, privileged demo queries. They preserve native identities,
per-item statuses, selectors and safe metadata without changing topology or grants.
Report acceptance is separate from bounded multipart completion; fragments cannot
renew the actual-send deadline or substitute for a missing native acknowledgement.
Durable sanitized evidence survives restart without automatic replay. See
[device-model queries](docs/architecture/ocpp201-remote-control.md#opt-in-read-only-device-model-queries)
for opt-ins, native limits, redaction, correlation and recovery. Command-result v1.5
and nested export v1.6 retain historical snapshots and existing target payload caps.
SQLite v12 migrates to v13, but an old v12 binary rejects v13: this feature does not
qualify automatic old-to-new-to-old rollback. Other device writes/monitoring,
certification and the complete-release gate remain outside that read-only capability.

OCPP 2.0.1 `SetVariables` and `SetNetworkProfile` are independently default-off,
privileged demo commands using **reference-only** public envelopes and a bounded,
owner-only private startup file. Exact resource/identity/slot/expiry binding is
checked before fresh admission and again at the actual socket send; neither values,
profiles nor reusable capabilities enter results or diagnostics. Native per-item
outcomes remain distinct from physical effects, and network `Accepted` means
stored/staged until a separate operator-controlled reboot. There is no automatic
Reset, migration, probing, splitting, retry or replay. Result v1.8, nested export
schemas v1.9 and runtime export revision 8 retain historical compatibility without
changing SQLite schema 14 or enabling global export/provider delivery.
See [protected device/network writes](docs/architecture/ocpp201-remote-control.md#opt-in-protected-device-and-network-writes)
and the [private provisioning and command examples](docs/operations/headless-cli.md#protected-ocpp-201-device-and-network-configuration).
Actual-daemon software-peer tests and an independent redacted-capture smoke exercised
these boundaries; they do not establish hardware interoperability, OCA certification,
production charging or a new browser configuration editor.

OCPP 2.0.1 `GetLocalListVersion`, protected `SendLocalList` and `ClearCache` are
independently default-off privileged demo station controls. The existing owner-only
startup file routes `list16:` and `list201:` capabilities to separately typed providers
for the exact configured station edition. Native 201 token/group/additional identifiers
retain original ASCII identifierString spelling and typed case-insensitive identity;
UTF-8 private messages and vendor metadata remain private and inert. OCPP 1.6 retains
its own historical Unicode and signed-version semantics. Native 201 updates require
positive i32 versions; explicit empty arrays are invalid, unlike omitted contents.
Value-free result v1.10, nested public export v1.11 and separately named runtime export
revision 10 preserve released schemas and SQLite14. No automatic mutation replay,
list ledger, production control, new browser editor or certification is implied.
See [native 201 boundaries](docs/architecture/ocpp201-remote-control.md#protected-native-local-authorization-list-and-cache)
and [private provisioning](docs/operations/headless-cli.md#protected-ocpp-201-station-authorization-list-and-cache).

OCPP 1.6 availability commands retain scheduled intent separately from committed connector and
station observations. See [OCPP 1.6 availability](docs/architecture/ocpp16-availability.md) for
scoped control, transaction completion, durable evidence, and reconnect behavior.

OCPP 2.0.1 remote commands preserve EVSE targeting, durable remote-start correlation and native
scheduled-reset evidence. See [OCPP 2.0.1 remote control](docs/architecture/ocpp201-remote-control.md)
for scoped dispatch, response/effect separation and restart behavior.

OCPP 2.0.1 availability retains station, EVSE and connector scope with durable scheduled intent.
See [OCPP 2.0.1 availability](docs/architecture/ocpp201-availability.md) for atomic observations,
independent connector state and conservative recovery.

Native report workflows can use [bounded multipart collection](docs/architecture/multipart-reports.md)
for correlated ordered fragments, shared memory admission, cancellation, and absolute deadlines.

Firmware and diagnostic providers can use [bounded artifact streaming](docs/architecture/artifact-streaming.md)
for byte transport and private temporary spooling with shared admission and cancellation.

Future industrial drivers remain behind the target registry and canonical data/command ports. See
the [industrial adapter extension boundary](docs/architecture/industrial-adapter-extension.md) for
the mapping checklist, unavailable first-release OPC UA kind, and compatibility limits.

Critical target deliveries remain owned by their original target instance and configuration
revision across restarts. See [target destination changes](docs/configuration/target-destination-changes.md)
for offline previews, restart-required state, audited archive/discard handling, and dispatch guards.
Configuration clients read the target catalog, validate and apply next-start target sections, and
authorize audited dispositions through the
[management configuration API](docs/configuration/management-target-configuration.md).

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

The opt-in [external CSMS smoke](docs/simulator/external-csms-smoke.md) runs the packaged
simulator against a pinned independent OCPP stack, with isolated dual-version charging and
remote-command evidence and explicit mismatch failures.

The simulator also offers an explicitly enabled [loopback control API](docs/simulator/control-api.md)
for bounded demo/staging scenario runs, scoped cancellation and pending-step fault controls.

The standalone simulator has a versioned deterministic TOML scenario contract and a machine-readable
JSONL runner. See the [scenario runner guide](docs/simulator/scenario-runner.md) for its actions,
failure categories, timeout model, and checked-in example.

The simulator's OCPP 1.6 charging example exercises registration, authorization, status,
transaction start/meter/stop, active-transaction reconnect, exact wire fixtures, and separate
remote-command acceptance without importing bridge state-machine code.

The OCPP 2.0.1 charging example exercises the corresponding native multi-EVSE flow with
`TransactionEvent` sequencing, complete meter-quality fields, reconnect continuity, exact
independent fixtures, and separate RequestStart/RequestStop acceptance.

The opt-in [Compose target demos](docs/testing/compose-profiles.md) build separate
daemon, simulator and target-client images and verify both OCPP editions against
generic MQTT, direct EMS HTTP (without a broker) or EMS MQTT. Use
`./scripts/test-compose-profiles.sh` for the bounded three-profile acceptance run;
`./scripts/compose-demo.sh browser mqtt` additionally exposes a disposable console
on a random **host-loopback-only** port. These are isolated nonproduction examples,
not PostgreSQL export or production charger deployments.

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

For a chronological production-only install with an independent supervisor status check
while the bridge is stopped, follow the [operations runbook](docs/operations/operations-runbook.md).
Use the [release recovery runbook](docs/operations/release-recovery-runbook.md) to distinguish
compatible application failure from storage/OS disasters, and the
[disposable rehearsal](docs/operations/runbook-rehearsal.md) for the existing testable boundary.

See [production and staging filesystem isolation](docs/operations/environment-filesystem-isolation.md)
for separate service accounts, units/slices, configuration, databases, runtime locks and journals,
with same-Pi and separate-Linux-host staging layouts.

See [staging data isolation](docs/operations/staging-data-isolation.md) for separate PostgreSQL
roles/databases and explicit, reidentified status-only imports into isolated test peers.

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

Native x86-64 and ARM64 runtime archives are built and smoke-tested separately from
the simulator. See [platform packages](docs/operations/platform-packages.md) for
installation, content auditing and the explicit Pi qualification boundary.
