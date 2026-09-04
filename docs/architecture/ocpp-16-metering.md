# OCPP 1.6J metering

`MeterValues` calls are decoded through the pinned OCPP 1.6J model before the protocol adapter
emits an application-owned measurement observation. The observation retains the native connector,
optional transaction ID, source timestamp, original decimal text, unit, measurand, phase, reading
context, location, and signed-data payload.

Raw decimal samples use exact base-ten values. Supported kilo-units are normalized to their base
unit without binary floating-point conversion. A syntactically valid OCPP sample containing a
non-decimal or out-of-range raw value remains observable as an unavailable value with explicit bad
quality; it is never changed to zero. Signed data remains unavailable with uncertain quality until
a separate signature policy verifies it.

Application reconciliation replaces a point by stable identity while retaining unrelated values.
The trusted host observation time is applied during reconciliation rather than copied from the
charger. Connector zero updates station-level values, unknown non-zero connectors fail explicitly,
and replaying the same samples after reconnect is idempotent in recovered snapshot state.
