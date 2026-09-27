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
schema files retain their content; repository line endings are normalized as a technical format
change. OCA specification material is copyright Open Charge Alliance and distributed under
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
