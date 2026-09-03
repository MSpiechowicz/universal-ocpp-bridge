# OCPP 2.0.1 metering

`MeterValues` and meter-bearing `TransactionEvent` calls are decoded only after typed OCPP
validation. The protocol adapter emits application-owned measurement observations and retains the
native EVSE/connector address, transaction identity and sequence number.

Each sample becomes an exact base-ten canonical value. The original decimal text, unit,
multiplier, measurand, phase, reading context, location, source timestamp and native protocol
reference remain attached as evidence. Signed meter structures are preserved verbatim as
structured JSON for later signature-policy work; the bridge does not claim to verify an unknown
signing scheme.

Application reconciliation replaces a point by stable point identity and keeps unrelated values.
The trusted host observation time is applied at reconciliation, not taken from the charger.
Replayed samples after reconnect are therefore idempotent in current snapshot state. Unknown
non-zero EVSE/connector references fail explicitly instead of creating topology implicitly.
