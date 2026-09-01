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
