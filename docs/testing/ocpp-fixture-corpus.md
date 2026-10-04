# Independent OCPP coverage and wire fixtures

The corpus under `tests/ocpp-fixtures/corpus` is an independent expected-result boundary. It is
static input to tests: the bridge encoder, protocol adapter, simulator client, and checker have no
command that creates or updates expected payloads. A contributor authors changes from the pinned
specification, records the new digest in `fixtures.json`, and submits both for ordinary review.

The initial executable subset covers charging-station-to-CSMS BootNotification, Heartbeat, and
transaction-start calls for OCPP 1.6J and 2.0.1. `inventory.json` records the complete first-release
feature inventory at a traceable level. `coverage.json` records protocol version, direction,
fixture/scenario IDs, externally observable behavior, and evidence status for every inventory ID.
Missing or duplicate rows, mismatched metadata, or a `verified` row without executable evidence
fail the checker. Planned work remains visible and intentionally prevents the release gate from
passing.

Run the development integrity check with:

```text
cargo run --package uob-ocpp-fixtures
```

Run the fail-closed release completeness gate with:

```text
cargo run --package uob-ocpp-fixtures -- --release
```

The second command is expected to fail until every applicable required inventory row has verified
project-owned evidence. Evidence from an external interoperability peer must use the distinct
`external_subset` status and cannot establish full release coverage. A genuinely inapplicable row
uses `not_applicable`, must match an inventory entry marked inapplicable, and must include a
rationale. Scenario references will become valid only when a checked scenario registry exists.

## Specification provenance and licensing

The authoritative sources are the Open Charge Alliance downloads recorded in `provenance.json`:

- OCPP 1.6 Edition 2 and its published errata bundle, including the OCPP-J Draft 4 schemas.
- OCPP 2.0.1 Edition 4, its June 2026 errata, and the Part 3 FINAL Draft 6 schema archive.

The source archive SHA-256 values pin acquisition; the fixture registry separately pins every
vendored schema and hand-authored wire file. CI never downloads a moving schema. The selected OCA
schema files retain their content; earlier additions normalize line endings as a technical
format change, while the GetCompositeSchedule pair and four native Set/Clear profile schemas
retain original CRLF bytes as noted below. OCA specification material is copyright Open
Charge Alliance and distributed under
Creative Commons Attribution-NoDerivatives 4.0. Before adding more source material, acquire it
from the recorded OCA download page, verify its archive digest, retain attribution, and confirm
that redistribution and any technical transformation comply with that license. Do not copy paid
certification-test content into this corpus.

Schema-valid serialization is only one layer of evidence. It does not prove application behavior,
complete protocol coverage, interoperability, resource safety, OCA certification, or measured
Raspberry Pi performance. Rows advance to verified only when their stated observable behavior has
the corresponding executable fixture and, where behavior is involved, independent scenario and
state evidence.

OCPP 1.6 GetConfiguration and ChangeConfiguration use the four unchanged request/response
schemas from the pinned Edition 2 archive and hand-authored wire fixtures. They cover
absent, empty and selected key lists; partial/unknown/read-only/omitted values; all
four native write statuses and rejected commands. The corpus proves wire shape,
while `ocpp16_configuration*` protocol integration tests exercise authenticated
admission, redaction, persistent outcomes, recovery and one-shot dispatch.

## OCPP 1.6 TriggerMessage fixtures

The TriggerMessage source is the OCA **OCPP 1.6 Edition 2 with published JSON
errata bundle dated 2025-04-29**, archive SHA-256
`2a1d80284ca60449e85951fc55bc0538b5d45bd6f97c6d9228745acacb52c11b`,
recorded in `provenance.json`. Four new schemas come from the archive's
`OCPP_1.6_documentation/schemas/json/TriggerMessage.json`,
`TriggerMessageResponse.json`, `DiagnosticsStatusNotification.json`, and
`FirmwareStatusNotification.json`. They retain the OCA content with only CRLF
normalized to LF, under OCA copyright and CC BY-ND 4.0. Existing corpus paths
for `BootNotification.json`, `Heartbeat.json`, `MeterValues.json`, and
`StatusNotification.json` are reused; they are not replaced or re-vendored.
The wire arrays are independently authored and do not reproduce OCTT material.

Fixture IDs beginning `wire.ocpp16.trigger-` contain eleven `TriggerMessage`
CALLs, three native CALLRESULTs (`trigger-accepted`, `trigger-rejected`,
`trigger-not-implemented`), and nine distinct station-originated `trigger-result-*`
CALLs. The requests name all six permitted `requestedMessage` values.
`trigger-status-station` addresses connector 0 only; `trigger-status-connector-1`
addresses connector 1; `trigger-status-all` omits the ID and expects status for
station 0 plus connectors 1 and 2. `trigger-meter-connector-{1,2}` addresses
one connector each; `trigger-meter-all` omits the ID and expects both connector
measurements, **not** a synthetic connector-0/all `MeterValues`. Both meter
results contain current example values for configured energy and voltage
measurands with `Trigger` context. `trigger-boot-irrelevant-connector` and
`trigger-heartbeat-irrelevant-connector` demonstrate that the message class
takes priority when a supplied connector ID is irrelevant.

Edition 2 §§5.17 and 6.51 define request scoping: omitted ID means all
applicable allowed IDs, whereas explicit 0 is specifically station status in
the §5.17 example; explicit positive IDs address a connector. §5.17 requires
the response before a subsequent requested CALL, allows the station to reject,
and says an Accepted response should result in a current message, not historic
data. The published OCPP-J errata restricts BootNotification triggers to
pre-Accepted registration (including pending): after Accepted the CSMS SHALL
NOT request another BootNotification until the station initiates a new boot.
The `trigger-boot` fixture represents only that permitted pre-acceptance case.
The `NotImplemented` fixture covers a station that does not implement a
schema-recognized class; unrecognized strings cannot validate against the OCA
request enum. The three reply fixtures share their corresponding request
unique IDs, while resulting CALLs have their **own** unique IDs: OCPP 1.6 does
not put the triggering request ID in a resulting CALL. Compatible observation
within the 60-second dispatch-start observation window is not proof of
causation or of success after reconnect/restart. CALLERROR is not included
because the corpus checker currently admits only CALL and CALLRESULT envelopes.

`ocpp16.remote-trigger` is `verified`: the checked outbound request fixtures
are paired with named real-socket protocol, persistent storage, running-service
and simulator behavior tests in `coverage.json`. Those tests exercise reply
ordering, one-shot dispatch, scope reconciliation, non-causal observation
outcomes and restart without replay. The row lists only outbound requests
because the inventory labels that requirement `csms_to_charging_station`;
the response and subsequent inbound CALL fixtures remain independently pinned
in `fixtures.json`. This does not establish interoperability with physical
stations, physical charging or OCA certification.

## OCPP 2.0.1 TriggerMessage fixtures

The 2.0.1 wire corpus uses unchanged OCA Edition 4 / June 2026 errata
Part 3 FINAL Draft 6 schemas from the pinned archive, SHA-256
`192482c82a5e27a2319d2142be2d8c074b68a22851ff5a12d0541efc1eda775a`.
The independently authored `wire.ocpp201.trigger-*` entries include 20
outbound TriggerMessage CALL fixtures for all eleven native classes, three
native statuses plus missing-connector rejection, and distinct schema-checked
station-originated CALLs. The `evse` object distinguishes EVSE and connector
targeting from 1.6's connector ID. Fixtures include irrelevant EVSE for
station-only classes, connector-specific StatusNotification, EVSE-wide
MeterValues, omitted/all targets, transaction events and certificate signing
requests. A schema-valid missing-connector or omitted-EVSE status trigger
fixture does not imply bridge admission without an exact connector scope.
Reply CALLRESULTs correlate with their request IDs; later station CALLs
have their own IDs and are not causally linked to native acceptance. The
certificate fixtures are wire examples, not
simulator private-key/CSR capability or certificate-chain/ISO 15118 workflows.

`ocpp201.remote-trigger` is `verified` by checked fixtures and named
authenticated protocol-socket, persistent SQLite storage, running-service
HTTP/WebSocket/SQLite and simulator tests in `coverage.json`. Its `fixture_ids`
list only the outbound direction declared by the inventory; independently
pinned native replies and resulting inbound calls remain in `fixtures.json`.
Those tests exercise scoped admission, preaccepted Boot, response ordering,
60-second non-causal reconciliation, denied/malformed/delayed/disconnect and
restart without automatic replay. An EVSE-scoped certificate receipt lacks
native EVSE identity and remains unattributable. The simulator returns
`NotImplemented` for certificate signing triggers without a private key/CSR;
verified here does not mean OCA certification, interoperability with physical
stations, physical charging, or completion of planned certificate workflows.

## OCPP 1.6 GetCompositeSchedule fixtures

The narrow `ocpp16.smart-charging.composite-schedule` row is `verified` and
`bidirectional`; the broad `ocpp16.smart-charging` profile requirement stays `planned`.
Its seven independently authored fixtures are `wire.ocpp16.composite-schedule-grid`,
`wire.ocpp16.composite-schedule-connector-a`, `wire.ocpp16.composite-schedule-connector-w`,
`wire.ocpp16.composite-schedule-accepted-a`, `wire.ocpp16.composite-schedule-accepted-w`,
`wire.ocpp16.composite-schedule-accepted-zero` and `wire.ocpp16.composite-schedule-rejected`.
They cover connector 0 grid aggregation, exact positive connector requests, omitted/forced
A/W units, meaningful periods and optional native metadata, genuine zero and native Rejected.

The official `schemas/1.6/GetCompositeSchedule.json` and
`schemas/1.6/GetCompositeScheduleResponse.json` are byte-for-byte copies from the pinned
OCA Edition 2 archive above, including their original **CRLF** line endings, under OCA
copyright and CC BY-ND 4.0. Unlike earlier LF-normalized additions, neither is transformed.
Verified source-member SHA-256 values are respectively
`e48c40527d9222d2c76ef83d6c6b9d2c1dcf34a98d722f2392353950d84e296a` and
`19e44ab05ce67421fa4f0017f97f7e5bc926f530c7af169131531a78fd1d12c3`;
the manifest also pins each independently authored wire file's digest.
The canonical request URI is `urn:OCPP:1.6:2019:12:GetCompositeScheduleRequest`.
Pinned semantics follow Edition 2 §§5.7, 6.21, 6.22, 7.13 and 7.14 and published
errata 3.24, 3.51 and 4.17.

`coverage.json` uses existing `fixture:` and `test:` evidence strings. `scenario_ids`
remains empty because no checked scenario registry exists. Malformed replies and CALLERROR
are **behavioral test inputs**, not schema-valid corpus fixtures: the checker admits only
valid CALL/CALLRESULT fixtures. Passed protocol scenarios include
`native_schedule_preserves_exact_rates_metadata_scope_and_zero_after_reopen`,
`validation::invalid_schedule_requests_and_insufficient_authority_send_no_call`,
`validation::malformed_native_schedule_is_uncertain_without_fabricated_evidence`,
`precision::exact_exponent_and_model_boundaries_never_round_native_rates`,
`lifecycle::delayed_schedule_reply_keeps_inbound_heartbeat_progress_and_duplicate_is_one_shot`,
`lifecycle::timeout_disconnect_callerror_and_late_reply_never_replay_after_restart` and
`lifecycle::interrupted_dispatch_recovers_uncertain_without_resending_schedule` in
`cargo test --locked -p uob-protocol-adapter --test ocpp16_composite_schedule`.

`cargo test --locked -p uob-service --test composite_schedule` passed
`native::normalizes_and_persists_native_schedule`,
`admission::denies_invalid_scope_and_authority_before_wire`,
`recovery::delayed_reply_keeps_heartbeat_progress` and
`recovery::disconnect_restart_never_replays_schedule_query` with an actual child daemon,
authenticated independent WebSocket peer, management HTTP, SQLite and process restart.
`cargo test --locked -p uob-storage-adapter --test composite_schedule` passed
`winning_schedule_survives_stale_conflicting_writers_effect_merging_and_reopen` and
`late_schedule_cannot_resolve_terminal_uncertainty_and_old_json_stays_readable`.
The row links these concrete tests for denied admission, delayed replies, invalid evidence,
duplicates, restart and reconnect rather than inventing a scenario registry.

A separate actual-daemon smoke observed exact high-tenth HTTP/SQLite evidence,
zero versus Rejected, heartbeat progress before a delayed reply, authenticated current and
historical EMS schemas, identical results after restart, no reconnect replay and a new
explicit query. This narrow evidence does not establish all smart charging, profile
installation/removal, local schedule calculation/enforcement, OCPP 2.0.1 schedules,
simulator smart charging, physical hardware interoperability or OCA certification.

## OCPP 1.6 native Set/Clear charging-profile fixtures

The narrow bidirectional `ocpp16.smart-charging.profiles` row is `verified` after scoped
executable regressions and actual-daemon smoke proof. The broad `ocpp16.smart-charging`
requirement remains `planned`; this does not qualify all smart charging or enforcement.
`scenario_ids` remains empty because there is no checked scenario registry. The row uses
existing `fixture:` and exact executable `test:` evidence symbols, not invented scenarios.

Four schema files are original-byte copies from the same pinned OCA OCPP 1.6 Edition 2
and published JSON errata bundle dated 2025-04-29, archive SHA-256
`2a1d80284ca60449e85951fc55bc0538b5d45bd6f97c6d9228745acacb52c11b`.
They retain original **CRLF** bytes, OCA copyright and Creative Commons
Attribution-NoDerivatives 4.0 attribution; no schema content or line endings are transformed.
The corpus paths and source-byte SHA-256 digests are:

| Schema under `schemas/1.6/` | SHA-256 |
|---|---|
| `SetChargingProfileRequest.json` | `fdd29c5d36a8118e462e7cf984c83ac7da8fb2b10ff285d923c016c47845df16` |
| `SetChargingProfileResponse.json` | `92c93950e87fa3a97684989656f9523c42805d3929c37dd033233dc0c0ba18c5` |
| `ClearChargingProfileRequest.json` | `22a84d99ab47e242c817a1cc3f34c07396a91d63b0987455a2202cd446b9485d` |
| `ClearChargingProfileResponse.json` | `4a576de2a37614998624984a86dcd9a42a6e6ecc51849b017e3a203dc1cce945` |

The eleven independently hand-authored fixture IDs (all prefixed `wire.ocpp16.`) are:

- Set CALLs: `profile-station-max`, `profile-recurring-default`, `profile-transaction-zero`.
- Set CALLRESULTs: `profile-set-accepted`, `profile-set-rejected`, `profile-set-notsupported`.
- Clear CALLs: `profile-clear-id-overrides`, `profile-clear-filter`, `profile-clear-all`.
- Clear CALLRESULTs: `profile-clear-accepted`, `profile-clear-unknown`.

`fixtures.json` pins each schema and wire-file digest. Expected wire JSON is not generated
by bridge/model encoders. Malformed replies and CALLERROR remain behavioral test inputs,
not valid corpus entries: the checker accepts only schema-valid CALL/CALLRESULT envelopes.
Native profiles preserve exact A/W tenths, genuine zero, signed IDs, optional native phases,
recurrence/validity/anchors and charger-truncated periods. ID overrides other Clear filters,
requiring station authority; child filter clear cannot use ID. Native Accepted alone proves
neither an installed-profile inventory nor charging enforcement.

`cargo test --locked -p uob-protocol-adapter --test ocpp16_charging_profiles` passed
eleven regressions, including:

- `exact_native_profiles_and_action_specific_statuses_survive_reopen`
- `independent_profile_fixtures_reach_native_status_boundary_without_station_widening`
- `precision::exactly_1024_periods_and_signed_identity_boundaries_are_preserved`
- `precision::native_optional_recurrence_and_expired_validity_remain_charger_decisions`
- `transactions::native_tx_profile_requires_unique_established_ongoing_identity_at_dispatch`
- `transactions::canonical_limit_never_acquires_full_profile_evidence_or_zero_semantics`
- `validation::profile_shape_precision_period_and_scope_fail_closed_before_wire`
- `validation::optins_and_privileged_permission_remain_independent_from_canonical_limit`
- `lifecycle::fully_encoded_profile_call_has_a_hard_bound_before_enqueue`
- `lifecycle::captured_request_is_immutable_during_snapshot_updates_and_duplicate_admission`
- `lifecycle::malformed_replies_errors_timeout_and_disconnect_remain_one_shot_after_reopen`

The passing storage tests are `terminal_profile_identity_status_and_effects_survive_conflicting_writers_and_reopen`
and `late_native_profile_reply_cannot_resolve_terminal_uncertainty` in
`cargo test --locked -p uob-storage-adapter --test charging_profile`.
`cargo test --locked -p uob-service --test charging_profiles` passed eight actual-process
tests. The selected coverage row links all eight exact symbols: native A/W/zero/status and
validity/scope preservation, independent opt-in/grants, no-CALL privilege/registration/
precision/ID-clear denial, malformed/CALLERROR/30-second timeout, delayed heartbeat progress,
and peer-apply disconnect/restart uncertainty without replay.

A separate rebuilt-daemon smoke with an independent RFC 6455 software peer observed A 8.1,
A 0, W 7200.1, Set Accepted/Rejected/NotSupported and Clear Accepted/Unknown in revision-6
typed results. It observed denied ID-clear/invalid precision/ordinary-control submissions
without a CALL, heartbeat progress before profile reply, retained uncertainty/no resend after
reconnect and daemon restart, eight retained terminal results, and reloaded peer profile state.
The peer's bounded persistence/replacement/filter behavior is software-only evidence, not
hardware enforcement, a full simulator, OCA certification or full release qualification.
Whole-workspace verification and fresh reviews are not claims of this narrow corpus row.
Separate real-daemon live browser proof covers existing-control compatibility with the new
profile descriptors, not nested profile editing or hardware behavior.

## OCPP 2.0.1 read-only device-model fixtures

The narrow bidirectional `ocpp201.device-model.queries` row covers
GetVariables, GetBaseReport, GetReport and their correlated NotifyReport exchange.
It is separate from the broad `ocpp201.device-model` row, which remains `planned`:
read-only queries do not complete variable writes, monitoring or every device-model
requirement. The complete-release gate remains fail-closed; narrow implementation
evidence is not OCA certification, physical-station interoperability or release
qualification.

Eight unchanged OCA request/response schemas for these four actions come from the
pinned Edition 4 / June 2026 errata Part 3 FINAL Draft 6 archive, SHA-256
`192482c82a5e27a2319d2142be2d8c074b68a22851ff5a12d0541efc1eda775a`.
They retain OCA copyright and CC BY-ND 4.0 attribution. The registry pins schema
and independently authored wire-file digests; neither bridge encoders nor tests
generate the expected wire payloads.

The `wire.ocpp201.get-variables-*` fixtures cover the safe DeviceDataCtrlr
request-limit identity, all five native per-item statuses and Accepted empty
values. The three `get-base-report-*inventory` requests cover native report bases;
`get-report-all` and `get-report-selectors` preserve omitted scope, selectors and
criteria rather than host-invented filtering. Separate `ack-getbasereport-*` and
`ack-getreport-*` fixtures retain Accepted, Rejected, NotSupported and EmptyResultSet.
`notify-report-prefix`, `notify-report-final` and `notify-report-empty-ack` retain
native requestId/seqNo/tbc/generatedAt, ordered metadata and the empty wire reply.
CALL unique IDs and native report request IDs are distinct.

Schema-valid fixtures prove wire shape, not authorization, secret disclosure,
ACK/report ordering or durable completion. Behavioral proof additionally needs
authenticated socket, storage and running-service evidence for no-wire denial,
exact/case-folded identities and result permutation, native partial statuses,
absent/empty/redacted values, unchanged signed IDs and shared cross-action reuse,
unknown/learned limits, report-before-ACK/ACK-before-report, nonrenewing actual-send
deadlines, ordering/count failures, bounded overflow and heartbeat progress,
client abandonment, restart/disconnect interruption and no replay. Malformed
native replies are behavioral inputs, not schema-valid corpus fixtures.

An independent authenticated peer against the actual daemon has observed the
narrow query/report behavior through management HTTP and private SQLite, including
privacy, failure counts, abandoned-client collection, actual-send timeout and
restart without replay. Current/historical EMS schema reads and existing MQTT
result publication exercise consumer boundaries separately. These observations
do not assert that the full workspace checks or final correctness/security reviews
are complete; `coverage.json` remains the machine-readable evidence status.

## OCPP 2.0.1 native Set/Clear charging-profile fixtures

The narrow bidirectional `ocpp201.smart-charging.profiles` row starts `planned`.
Only observed behavioral evidence can advance it; the broad `ocpp201.smart-charging`
row remains `planned`. `scenario_ids` is empty because no checked scenario registry
exists. Static fixtures contain valid CALL/CALLRESULT envelopes, not malformed
requests, CALLERRORs or assertions of physical charging.

The four files below are byte-for-byte copies of the exact members under
`OCPP-2.0.1_part3_JSON_schemas/` in the pinned Part 3 schema ZIP. They retain
original CRLF bytes and OCA copyright/CC BY-ND 4.0 attribution. The outer archive
SHA-256 is `192482c82a5e27a2319d2142be2d8c074b68a22851ff5a12d0541efc1eda775a`;
the schema ZIP SHA-256 is
`6279c40b74929cce7fca439622194a8890a0d250de5665f3e4d80ef8529c51ce`.
The existing `provenance.json` source entry already pins both archives.

| Schema under `schemas/2.0.1/` | Original-byte SHA-256 |
| --- | --- |
| `SetChargingProfileRequest.json` | `fd905c14648a36fec256da24e4f2befd75f20051bbb2c9bf4985ebbe96f3c497` |
| `SetChargingProfileResponse.json` | `3b8db1dad2ec907c1aa8a5550b8e2c35a130a27ef58774c8c7d334056f24b175` |
| `ClearChargingProfileRequest.json` | `9a9866944e376a7d04ef9340432a3bd9772f7d25ea7de936ab5f764ef4ec9bc6` |
| `ClearChargingProfileResponse.json` | `db7d11e02cb27c634b665f0b9995f10416a2ff420495f07a82a4b0b03c4a36c2` |

The 18 independently authored `wire.ocpp201.profile-*` fixtures preserve native
`evseId`, signed profile/schedule IDs and nested `chargingProfileCriteria`.
Set fixtures include station maximum, station Daily default, positive-EVSE Weekly
default, Relative transaction zero and an Absolute transaction with one-phase
selection on EVSE 1. Both A/W, optional validity anchors, duration truncation,
omitted optional phases and exact tenths are represented. Positive phase selection
is conditional on exact-EVSE native capability proof; a fixture alone cannot grant
that proof. The schema-valid external-constraints Set fixture is deliberately
inadmissible as a CSMS command. Clear fixtures include ID-only, nested AND filters,
station-zero scope, three all-EVSE purpose-only baseline clears, nonmatching ID
and protected external purpose. Reply fixtures distinguish Set Accepted/Rejected
and Clear Accepted/Unknown. The native profile and schedule are not OCPP 1.6
`csChargingProfiles`/`chargingSchedule` objects, and an empty clear is not accepted.

### Independent persistent software peer

`bins/uob-service/tests/charging_profiles201/peer.rs` and its children implement
one raw-wire peer reused by the test-tool-only `profile201_wire_peer` example.
It imports no bridge profile model or production validator. Its own 128-profile
inventory retains native JSON numbers, does not compute physical power or a
composite schedule, replaces only the same profile ID, applies clear selectors
with AND, and protects locally seeded external constraints. Independent validation
checks transaction/EVSE association, first-zero ordered periods, exact nonnegative
tenths, kinds/recurrence/anchors/validity and positive exact-EVSE phase capability.
Denied or unmatched operations leave persistent state unchanged. Profiles are
written to an owned private file by a synced atomic replacement and reopened on
peer restart; malformed, oversized, nonprivate or symlinked state fails closed.
Runtime transaction associations and phase capability maps are independently
established, not fabricated from persisted bridge outcomes.

To prepare an actual-process smoke, build with:

```text
cargo build --locked -p uob-service --example profile201_wire_peer
target/debug/examples/profile201_wire_peer /private/directory/peer.json
```

The sole argument is a private configuration path, not a credential. Configuration
is JSON with `url` (loopback `ws` only), `state_file`, optional `authorization_file`
(private file containing the exact HTTP Authorization header), `phase_evse`
(default 1) and `phase_supported` (default false). The state parent directory must
be owned and private; configuration/credential/state files must deny group/other
access. The process performs an explicit native PowerUp boot and requires Accepted.
It does not seed transactions or automatically clear existing policies.

stdin accepts bounded JSON controls: `{"action":"inspect"}`,
`{"action":"phase","evse":1,"supported":true}`,
`{"action":"transaction","evse":1,"id":"native-transaction","ended":false}`,
`{"action":"heartbeat"}`,
`{"action":"controls","reject_next":true,"delay_ms":1000,"disconnect_after_apply":false}`,
`{"action":"reconnect"}` and `{"action":"quit"}`. Transactions send explicit
native Started/Ended events; issue them when no other response is pending.
Delay is bounded to 30 seconds and pending acknowledgements to 128; stdin and
heartbeat responses continue while acknowledgements wait. Disconnect-after-apply
persists the actual operation and closes before its acknowledgement. Reconnect
or a separate process restart reopens inventory and performs a new boot.

stdout emits sanitized fixed event labels and `inspect` includes native numeric
profile/schedule/EVSE/rate metadata and incoming Set/Clear/GetVariables counters.
It never prints credentials, transaction strings or raw server diagnostics.
Counters are connection-local: inspect before/after a duplicate or denied command
to observe no resend; reopen and inspect actual installed profiles to distinguish
charger-side persistence from bridge result/ledger persistence. Named peer
regressions cover replacement/reopen, selective clear/no-match, transaction/phase
denial, mixed-selector errors, external protection, capacity and boolean query
scope. Verification observed all six native protocol regressions and ten durable
ownership regressions passing. An actual `uob daemon` plus this separate peer
passed nine smoke checks: explicit three-purpose initialization, exact EVSE and
transaction authority, zero and decimal rates, omitted fields and periods at the
duration boundary, replacement, denial without mutation, duplicate suppression,
heartbeat progress during a delayed Set, AND-selective Clear, and disconnect/
restart persistence without replay. Explicit recovery Clear reconciled the
uncertain profile before a later Set.

The narrow `ocpp201.smart-charging.profiles` corpus row is verified; the broad
smart-charging row remains planned. Peer state and protocol ACKs do not establish
physical charging effects, hardware enforcement, certification, full
smart-charging coverage or product-simulator functionality.

## OCPP 2.0.1 native local authorization

The corpus imports the six unmodified SendLocalList/GetLocalListVersion/ClearCache
request/response schemas from the existing OCA Edition 4 archive pin. Fourteen
independent golden wire fixtures cover native Full, Differential upsert/delete,
omitted contents, zero/positive query versions and exact native response statuses.
`local-authorization201-requirements.json` binds Part 2 D01/D02/C10/C11 semantics,
the primitive/conditional-field tables and independent assertion sources to the
archive/schema ZIP hashes. The native authorization coverage rows remain **planned**
until the integration owner observes their checks; fixture presence is not verification.

The independent negative corpus separates JSON-schema validity from station semantic
acceptance: empty arrays, nulls, integer bounds, duplicate typed identity, limits,
metadata, native ASCII primitives and NoAuthorization conditional empty identity.
The Full-only required information property is checked separately because the pinned
schema leaves that property optional. No fixture rewrites explicit empty arrays into
omission or borrows 1.6 NotSupported/-1 semantics.

An additional independently authored native NotifyReport fixture distinguishes
LocalAuthListCtrlr.Entries Actual count (`1`) from variableCharacteristics.maxLimit
capacity (`256`). The compiled simulator process regression sends real native
GetBaseReport/GetReport requests, observes actual ACK/NotifyReport pairs before
and after installation, acknowledges reports, and checks caseless root identities.
The joint smoke Authorize helper reuses independently authored
`wire.ocpp201.authorization.valid` (Local token), never bypassing fixture binding.

Authored independent simulator tests cover atomic/private persistence, native typed
list priority, latest nonaccepted cache refresh, cache eviction and explicit expiry,
actual WebSocket delayed/dropped ACKs, process death/denied Boot recovery and uncertain
original TransactionEvent retention without automatic retry. The opt-in
`bins/uob-sim/tests/native201_joint_smoke.py` runs actual separate daemon/simulator
processes, real authorization-backed cache population, cache-only clearing, delayed/
dropped mutating ACK, actual service restart/no native mutation replay, killed simulator
list/cache recovery, fresh native query and actual SQLite/WAL/output privacy checks.
Its transparent TCP observer never supplies fake replies or seeds station state.

Use the [runnable commands and software-boundary limitations](../simulator/local-authorization201.md).
Authored assertions, source inspection and pins are not charger hardware qualification,
OCA certification, full local-authorization conformance or evidence of executed tests.

