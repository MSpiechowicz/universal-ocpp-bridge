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

## OCPP 1.6 firmware fixtures

Two sources are pinned in `provenance.json`.

- **OCPP 1.6 archive.** `UpdateFirmware.json`, `UpdateFirmwareResponse.json` and
  `FirmwareStatusNotificationResponse.json` come from the archive above. They keep their
  original CRLF bytes, like the GetCompositeSchedule pair. The existing
  `FirmwareStatusNotification.json` copy is reused.
- **Security Whitepaper.** The four signed schemas come from **Improved security for
  OCPP 1.6-J, Edition 4 (2026-02-05)**:
  - Its OCA download is recorded as provenance version `1.6-security-whitepaper`, archive
    SHA-256 `158b883e8ee712fd80fad3610b69fad355f7831d7a0ccdd996ce6b5452ce6859`.
  - The inner `JSON_schemas.zip` has SHA-256
    `5c69b84a5b4efe99d90a6c432034f726894c51a02203c274c00c361caf15a889`.
  - The schemas are `SignedUpdateFirmware.json`, `SignedUpdateFirmwareResponse.json`,
    `SignedFirmwareStatusNotification.json` and
    `SignedFirmwareStatusNotificationResponse.json`.
  - They are byte-identical, LF and Draft 6, under `schemas/1.6-security/`.

Both sources are OCA copyright under CC BY-ND 4.0.

The 34 fixture IDs beginning with `wire.ocpp16.firmware-` and
`wire.ocpp16.signed-firmware-` were authored independently. They cover:

- both requests in full and minimal form, including the signed i32 minimum `requestId`;
- the empty legacy acknowledgement and all five signed replies;
- all seven legacy statuses and all fourteen signed statuses, each with `requestId`;
- the identity-free signed `Idle` permitted by L01.FR.21;
- both empty notification replies.

`wire/1.6/firmware-negative-cases.json` separates the OCA schema floor from native semantics
with 14 cases. Several are schema-valid but refused natively:

- a non-`Idle` signed status without `requestId`;
- a `requestId` outside i32;
- negative `retries`.

The rest violate the schemas:

- the `InvalidCertificate` firmware status that Edition 4 removed;
- signed-only values in the legacy notification;
- a location over 512 characters and a signature over 800 characters;
- a missing signing certificate (L01.FR.11).

`firmware16-requirements.json` maps the 1.6 §4.5/§5.19 and errata items and the L01
requirement IDs to fixtures and tests.

The new `ocpp16.firmware-diagnostics.firmware-update` row is `verified`. Its evidence
comes from:

- the bridge decoder and the independent simulator model, which agree with every
  corpus fixture and negative case;
- storage and actual-daemon tests;
- an opt-in separate-process joint smoke.

The broad `ocpp16.firmware-diagnostics` row stays `planned` until diagnostics are
implemented. Security events, `ExtendedTriggerMessage` and certificate revocation are not
claimed.

## OCPP 1.6 diagnostics and log fixtures

The schemas come from the two sources pinned for firmware:

- **OCPP 1.6 archive.** `GetDiagnostics.json`, `GetDiagnosticsResponse.json` and
  `DiagnosticsStatusNotificationResponse.json` keep their original CRLF bytes. The existing
  LF-normalized `DiagnosticsStatusNotification.json` copy is reused unchanged, because the
  TriggerMessage fixtures pin its digest.
- **Security Whitepaper Edition 4.** `GetLog.json`, `GetLogResponse.json`,
  `LogStatusNotification.json` and `LogStatusNotificationResponse.json` are byte-identical
  LF Draft 6 copies from its `JSON_schemas.zip`, under `schemas/1.6-security/`.

The 26 fixture IDs beginning with `wire.ocpp16.diagnostics-` and `wire.ocpp16.log-` were
authored independently. They cover:

- `GetDiagnostics` in full (time window, retries) and minimal form, and its reply with and
  without `fileName` (no `fileName` means no diagnostics are available, §5.9);
- all four `DiagnosticsStatusNotification` statuses and the empty reply;
- `GetLog` for `DiagnosticsLog` and `SecurityLog`, each in full and minimal form, with the
  i32 maximum and minimum `requestId`;
- the `Accepted` (with and without `filename`), `Rejected` and `AcceptedCanceled` replies;
- all seven `LogStatusNotification` statuses with `requestId`, the identity-free `Idle` that
  N01.FR.12 permits, and the empty reply.

`wire/1.6/diagnostics-negative-cases.json` holds 24 cases. CALLRESULT cases carry a
`schema` key because their frame names no action. Schema-valid cases that are still refused
natively:

- a non-`Idle` `LogStatusNotification` without `requestId` (N01.FR.12);
- a `requestId` outside i32;
- negative `retries` or `retryInterval`;
- an inverted `startTime`/`stopTime` or `oldestTimestamp`/`latestTimestamp` window;
- a `fileName` with a control character (CiString is printable ASCII).

The schema-invalid cases include:

- `AcceptedCanceled` as an upload status. N01.FR.20 names it, but `UploadLogStatusEnumType`
  does not contain it.
- the log-only failure statuses in the legacy notification;
- a `remoteLocation` over 512 characters and file names over 255 characters;
- missing `location`, `log`, `requestId` or reply `status`;
- an unknown `logType`, and a `status` in the legacy reply.

`diagnostics16-requirements.json` maps 1.6 §4.4, §5.9, §6.17/6.18, §6.25/6.26 and §7.24 and
the N01 requirement IDs to fixtures and tests. The new
`ocpp16.firmware-diagnostics.diagnostics-logs` row stays `planned` until the bridge,
simulator and joint-smoke evidence it names exists. The broad `ocpp16.firmware-diagnostics`
row is unchanged.

## OCPP 2.0.1 firmware fixtures

`UpdateFirmwareRequest.json`, `UpdateFirmwareResponse.json` and
`FirmwareStatusNotificationResponse.json` were added to `schemas/2.0.1/` byte-identical
(LF, Draft 6) from the pinned 2.0.1 Part 3 archive in `provenance.json`. The existing
`FirmwareStatusNotificationRequest.json` copy is reused unchanged; it carries one trailing
newline that the archive member lacks, and its recorded SHA-256 is the one fixtures cite.

The 26 fixture IDs beginning with `wire.ocpp201.firmware-` were authored independently from
use cases L01 and L02, Figure 116 and errata 2.14 (L01.FR.04). They cover:

- one `UpdateFirmware` message in secure form (with `signingCertificate` and `signature`),
  non-secure form, and minimal forms of both;
- all five `UpdateFirmwareStatusEnumType` replies and a `Rejected` reply with `statusInfo`;
- all fourteen `FirmwareStatusEnumType` notifications, each with the same `requestId`
  (L01.FR.10);
- the identity-free `Idle` permitted by L01.FR.20;
- the empty notification reply.

`wire/2.0.1/firmware-negative-cases.json` separates the OCA schema floor from native
semantics with 17 cases. Several are schema-valid but refused natively:

- a non-`Idle` status without `requestId` (L01.FR.20);
- a `requestId` outside i32;
- a signature without its signing certificate (L01.FR.11/12);
- an installation time before retrieval, and negative `retries`, which the bridge never
  sends.

The rest violate the schemas: an unknown or lowercase status, extra fields, wrong
`requestId` types, a missing `requestId` or `retrieveDateTime`, and a location, signature
or certificate over its length limit.

`firmware201-requirements.json` maps the L01/L02 requirement IDs to fixtures and tests.

The new `ocpp201.firmware-diagnostics.firmware-update` row is `verified`. Its evidence
comes from:

- the bridge decoder and the independent simulator model, which agree with every corpus
  fixture and negative case;
- storage and actual-daemon tests;
- an opt-in separate-process joint smoke with one secure and one non-secure station.

The broad `ocpp201.firmware-diagnostics` row stays `planned` until diagnostics (#125) are
implemented. Security events (#128), L03/L04 local-controller publishing and certificate
revocation are not claimed.

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

## OCPP 2.0.1 composite schedule and installed-profile report fixtures

The narrow bidirectional `ocpp201.smart-charging.composite-schedule` (K08) and
`ocpp201.smart-charging.profile-reports` (K09) rows are `verified`; the broad
`ocpp201.smart-charging` row remains `planned`. `scenario_ids` is empty because no checked
scenario registry exists. Static fixtures contain only valid CALL/CALLRESULT envelopes;
malformed replies, out-of-query fragments and CALLERRORs are behavioral test inputs.

The six files below are byte-for-byte copies of the exact members under
`OCPP-2.0.1_part3_JSON_schemas/` in the pinned Part 3 schema ZIP, retaining original CRLF
bytes and OCA copyright/CC BY-ND 4.0 attribution. The outer archive SHA-256 is
`192482c82a5e27a2319d2142be2d8c074b68a22851ff5a12d0541efc1eda775a`; the schema ZIP SHA-256 is
`6279c40b74929cce7fca439622194a8890a0d250de5665f3e4d80ef8529c51ce`. The existing
`provenance.json` source entry already pins both archives.

| Schema under `schemas/2.0.1/` | Original-byte SHA-256 |
| --- | --- |
| `GetCompositeScheduleRequest.json` | `1b8343197505b1ae3d78b2006d709d5784840ccaa39cb1e5ebcd26a37afaad4c` |
| `GetCompositeScheduleResponse.json` | `1167b0875328eef0b02567c86c51778992acf698b4f6669e5d07a3b26b1961c2` |
| `GetChargingProfilesRequest.json` | `a08efa0af98bd85b7b172295d79d59d3a7b19c3e258b04c00985c0ce5d899921` |
| `GetChargingProfilesResponse.json` | `2ccb8d6f82f99a7885982cb3609abb8050c021d2713fd62ce9045e758a2506a2` |
| `ReportChargingProfilesRequest.json` | `4b0e2cb832c04e1e293a1d5fe979c968c2f00298f0675868795da3cbe678d110` |
| `ReportChargingProfilesResponse.json` | `f2b14ea0ccce131f0aef3e53cd3a2996b68494cf7cade5c1d83564d5ad66ded6` |

Twelve independently authored wire fixtures cover the grid and EVSE composite requests,
an Accepted grid schedule with exact tenths and zero, an offset-timestamped single-phase
EVSE schedule with `phaseToUse`, Rejected with `UnsupportedRateUnit`, ID-list and
source/purpose GetChargingProfiles requests, Accepted and NoProfiles replies, a station
fragment with `tbc`, a final EVSE fragment with two schedules and a `salesTariff`, and the
empty report acknowledgement. `ocpp201_schedules::fixtures` replays each one over an
actual authenticated socket. `smart-charging201-requirements.json` maps K08.FR.01-07 and
K09.FR.01-06 to these fixtures and tests, citing the Edition 4 Part 2 specification
(SHA-256 `2bd854c01abdf20290d016e05779f952655177f09fedff2149126921dd5557e2`), the June
2026 errata (no K08/K09 items) and Appendices v1.5 reason codes. Charger-side calculation
(K08.FR.02/04/06) remains the charger's obligation; the mapping covers only CSMS-side
behavior and is not OCA certification or hardware interoperability.


## OCPP 2.0.1 charging-needs and external-limit fixtures

Two narrow bidirectional rows are `verified`:

- `ocpp201.smart-charging.external-limits` covers K11–K14.
- `ocpp201.smart-charging.ev-negotiation` covers charging needs and EV schedules in K15–K17.

The broad `ocpp201.smart-charging` row stays `planned`. For example, charging profiles
embedded in a remote start (K05) are still unsupported. `scenario_ids` is empty because
there is no checked scenario registry yet.

The eight files below are byte-for-byte copies of the matching members under
`OCPP-2.0.1_part3_JSON_schemas/` in the pinned Part 3 schema ZIP. They keep the OCA
copyright and CC BY-ND 4.0 attribution. `provenance.json` already pins both archives.

| Schema under `schemas/2.0.1/` | Original-byte SHA-256 |
| --- | --- |
| `NotifyEVChargingNeedsRequest.json` | `9084c8d497ce67d7b23d929a5656810e0e6c7ecde61414ba4c3ee1def02bf5e6` |
| `NotifyEVChargingNeedsResponse.json` | `835923db28d011b189e0a4b28bbfeb142f81bd09e712646dc9de9f133283fafa` |
| `NotifyEVChargingScheduleRequest.json` | `29f99ee6b94da9137c3af11d2dae9dd2b80b158d0fa7b682eef74af6299bf885` |
| `NotifyEVChargingScheduleResponse.json` | `864a530bc32e7c695773f854c832a5ec5ce688a5f277afba1d6463cf9088eb15` |
| `NotifyChargingLimitRequest.json` | `577073990a9d07051df73c473671428ed6f4a1ff300b93af2b9d2a46d01115cb` |
| `NotifyChargingLimitResponse.json` | `617c45cdf576e0aab55d9cb7631862b5367e41139718d71faaa5a41748364b9c` |
| `ClearedChargingLimitRequest.json` | `88ebc9a55eb308de3cf1dd2c664faecaca8fa24737a9c18765d20da5f4d42738` |
| `ClearedChargingLimitResponse.json` | `6ed44859833ecf021146ffee90400e8d3ee28d2b5868233c5242474e5334dccf` |

Thirteen independently authored wire fixtures cover:

- AC three-phase and DC charging needs, with the `Processing` answer and the `Rejected`
  answer with `NotEnabled`.
- An ampere EV schedule with exact tenths and a genuine zero, with its `Accepted` answer
  and its `Rejected` answer with `ValueTooHigh`.
- A grid-critical `SO` limit at the grid connection with a watt schedule, and an
  `EMS` limit on EVSE 1 without schedules.
- Grid and EVSE releases, and the empty acknowledgements.

`charging-negotiation-negative-cases.json` records 19 more CALLs that native semantics
refuse. Most of them pass the pinned schema floor, for example a CSO-sourced limit, a zero
EVSE, a mismatched or missing parameter set, unordered periods and two fractional digits.
`uob-ocpp-fixtures` checks each recorded `schema_valid` flag against the pinned schema, and
the protocol adapter refuses every case.

`smart-charging201-requirements.json` maps the CSMS-side K11–K17 requirements to these
fixtures and to the protocol, storage and actual-daemon tests. Station and
local-controller obligations, and the profile the operator's EMS must send, are named as
outside the bridge's own behavior. The June 2026 errata has no K11–K17 items. This is
software evidence only, not ISO 15118, hardware interoperability or OCA certification.
