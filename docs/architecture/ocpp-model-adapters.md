# OCPP model adapter boundary

The production service pins `rust-ocpp` 3.0.4 with default features disabled and only the
`v1_6` and `v2_0_1` model features enabled. OCPP 2.1 and SOAP are outside the first release and
are rejected during protocol negotiation rather than treated as compatible variants.

Concrete model types are confined to `uob-protocol-adapter`. Each negotiated edition has a
separate decoder that validates the CALL envelope, selects an explicitly implemented action,
deserializes its payload into the pinned model type, performs available field validation, and
maps the result into `uob_application::ChargerObservation`. The application contract retains the
protocol edition and native connector/EVSE reference without depending on `rust-ocpp`.

An action is not supported merely because the model crate can deserialize it. Actions remain
`NotImplemented` until their feature work adds application orchestration and behavioral tests.
Malformed frames and invalid payloads map to sanitized, version-qualified CALLERROR details.

## Validation and specification gaps

The independently authored corpus under `tests/ocpp-fixtures` remains the pinned schema evidence.
Adapter tests compile and decode its representative BootNotification, Heartbeat, and transaction
start frames for both editions. The model crate supplies `validator` rules for OCPP 1.6 request
types but does not expose equivalent validation uniformly for every OCPP 2.0.1 request. The 2.0.1
adapter therefore adds semantic checks for the mapped fields; fixture/schema verification stays a
separate mandatory check. This boundary is not an OCA certification claim and does not imply that
unmapped model modules have working service behavior.

## OCPP 1.6J DataTransfer

`uob_application::data_transfer::Registry` installs at most 64 exact vendor/message
capabilities. An omitted `messageId` is its own capability, not a wildcard. Unknown vendors
return `UnknownVendorId` without data; known vendors with unmatched messages return
`UnknownMessageId`. Only a registered provider can return `Accepted` or `Rejected`.
There is no built-in production vendor implementation or implicit interpretation of opaque data.

Requests follow the pinned OCPP 1.6 Edition 2/JSON errata schemas: vendor IDs have at most
255 characters, message IDs 50, and optional fields may be omitted but not null. Empty strings
remain schema-valid. Request and response data are additionally bounded to 16 KiB of UTF-8
bytes as this bridge's vendor-agreement limit, including unknown vendors. Oversized, mistyped,
or additional properties produce a sanitized CALLERROR rather than successful acceptance.

The authenticated station's ordered owner calls `v16::data_transfer::complete_data_transfer`
for decoded incoming calls. Accepted OCPP 1.6 registration is required. Provider evaluation is
side-effect-free and has a five-second deadline; the subsequent atomic SQLite commit is not
cancelled by that deadline. A correlated CALLRESULT is available only after persistence.
The snapshot retains fixed-size last-status, saturating receipt count, and request/response
content-byte counts (including identifier/status bytes), not vendor identifiers or opaque data.
The provider's `Accepted` response does not invent a charging-state transition.

`v16::data_transfer::send_data_transfer` is the corresponding low-level CSMS-originated API.
Its embedding caller owns authorization, exact station ownership, fresh message IDs and
serialization with other snapshot updates; it is not a management API or an authorization
bypass. It requires the same exact local capability registration and authenticated socket
identity. It persists `TransmissionUncertain` before enqueueing, sends once through the bounded
call lifecycle, and commits the native status or timeout/disconnect outcome before returning.
Response data is returned as bounded `OpaqueData` only to the caller. `UnknownVendorId` with
response data is rejected, as required by OCPP §4.3; `UnknownMessageId` may contain bounded data.
Neither reconnect nor process restart automatically replays a transfer.

`OpaqueData` omits its contents from `Debug`; default flow diagnostics and persisted receipt
state contain no opaque payload. Vendor-specific effects, external retries, and certification
are outside this boundary. A vendor integration needing side effects must supply its own
reviewed orchestration rather than perform them in the cancellable provider evaluator.

Verification:

```text
cargo test --locked -p uob-protocol-adapter --test ocpp16_data_transfer
cargo run --locked --quiet -p uob-ocpp-fixtures
```

The independent schema-checked wire corpus and real-WebSocket/SQLite scenarios cover native
status mapping, exact capabilities, invalid input, delayed and unavailable providers, failed
commits, redacted diagnostics, outbound malformed replies, timeout, disconnect and recovery.

## OCPP 2.0.1 DataTransfer

`uob_application::data_transfer201::Registry` and `v201::data_transfer` provide the corresponding
edition-specific application and wire boundary. The existing OCPP 1.6J API remains separate:
accepted 1.6 registration cannot authorize a 2.0.1 transfer. Both directions require a current,
accepted 2.0.1 registration; outbound delivery also checks the authenticated socket's station
identity and negotiated edition.

OCPP 2.0.1 `data` is arbitrary JSON, not a string-only 1.6 payload. Objects, arrays, scalars and
explicit JSON `null` retain their meaning; omitted data stays absent. Each opaque `data` or
`customData` value is limited to 16 KiB of compact serialized UTF-8 JSON, including quotes and
escapes. Its size is counted without allocating another payload buffer and cached for receipt
accounting. The shared WebSocket message budget still applies to the complete envelope.
`customData` requires a string `vendorId` of at most 255 characters and permits arbitrary
extension members. Optional `statusInfo` requires `reasonCode` (at most 20 characters), permits
`additionalInfo` (at most 512 characters), and may contain bounded `customData`. Other
request/response/statusInfo members are rejected. Opaque values, request identifiers and
status-detail text are redacted from these application types' `Debug` output.

Capabilities remain exact vendor/message pairs, including distinct omitted and empty message
identifiers. Unknown vendors/messages receive native unknown statuses; only an explicitly
installed provider can accept or reject a request. There is no default production vendor
implementation. Providers receive validated data and extensions without any claim that the bridge
understands their semantics. Unlike the 1.6 boundary, 2.0.1 native unknown-status responses may
carry bounded JSON data; its pinned specification/schema has no status-dependent prohibition.

`complete_data_transfer` exposes a correlated reply only after an atomic receipt commit. The
five-second timeout applies to provider evaluation, not the authoritative write. Snapshots retain
only `ocpp201/data-transfer/` status, saturating receipt count, content-byte counts, and outbound
status. Counts include identifier/status bytes and opaque JSON bytes, but exclude the enclosing
request/response field-name overhead.
No vendor identifier, data, extension, or status-detail text is persisted. Failed writes leave
the caller's snapshot unchanged.

`send_data_transfer` is an embedding API, not a management command ingress. Its ordered station
owner must authorize the operation, allocate a fresh message ID, and serialize snapshot updates.
It commits `TransmissionUncertain` before enqueueing, then a native reply or terminal lifecycle
outcome before returning. Malformed replies remain uncertain rather than becoming successful.
Timeout, disconnect, reopen, and reconnect never trigger an automatic vendor-operation replay.

### OCPP 2.0.1 requirement evidence

The independently authored corpus uses the provenance-pinned Edition 4 Part 3 schemas. Part 2
section P requirements map to the executable `ocpp201_data_transfer` suite:

| Requirements | Boundary and behavioral evidence |
|---|---|
| P01.FR.01 / P02.FR.02 | Vendor extension boundary only; no built-in interpretation or command-policy bypass. Exact capability admission rejects unsupported outbound requests without sending. |
| P01.FR.02–03 / P02.FR.03–04 | Vendor identifiers retain schema-valid values; reversed DNS is a recommendation, not an extra rejection rule. Omitted/empty message IDs route independently; Unicode limits are verified. |
| P01.FR.04 / P02.FR.05 | Explicit 16 KiB vendor-agreement limit; invalid/oversized values and escaped JSON byte boundaries cannot mutate persisted state. |
| P01.FR.05–06 / P02.FR.06–07 | Unknown-vendor precedence and exact message matching retain native statuses in fixtures, persisted receipts, and observed replies. |
| P01.FR.07 / P02.FR.08 | Test-only vendor agreement controls Accepted/Rejected and JSON data. Real-wire replies retain customData/statusInfo; debug, flow records, and durable receipts exclude sensitive content. |

SQLite reopen, wrong-edition registration, failed writes, provider timeout, delayed wire replies,
outbound CALLERROR/malformed replies, disconnect, and no-replay recovery provide the lifecycle
evidence beyond schema validation. This is not certification or closure of other planned features.

```text
cargo test --locked -p uob-protocol-adapter --test ocpp201_data_transfer
cargo run --locked --quiet -p uob-ocpp-fixtures
```
