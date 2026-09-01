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
