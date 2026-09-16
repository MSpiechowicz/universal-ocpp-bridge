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
state contain no opaque payload. Vendor-specific effects, external retries, OCPP 2.0.1
DataTransfer, and certification are outside this boundary. A vendor integration needing
side effects must supply its own reviewed orchestration rather than perform them in the
cancellable provider evaluator.

Verification:

```text
cargo test --locked -p uob-protocol-adapter --test ocpp16_data_transfer
cargo run --locked --quiet -p uob-ocpp-fixtures
```

The independent schema-checked wire corpus and real-WebSocket/SQLite scenarios cover native
status mapping, exact capabilities, invalid input, delayed and unavailable providers, failed
commits, redacted diagnostics, outbound malformed replies, timeout, disconnect and recovery.
