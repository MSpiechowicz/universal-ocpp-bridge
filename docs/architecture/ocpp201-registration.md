# OCPP 2.0.1 registration and status lifecycle

`v201::complete_registration` handles decoded calls from the bounded authenticated OCPP session;
`v201::registration_call` accepts complete wire frames. The station's existing ordered owner supplies
its authoritative restored snapshot, storage port, server receipt time, explicit Accepted/Pending/
Rejected registration policy decision, and nonzero heartbeat/retry interval. It sends the returned
payload through the existing responder. Every successful transition commits before replacing the
in-memory snapshot or returning a reply. Storage failures return InternalError.

Boot replaces bounded `ocpp201/registration/` points for status, interval_seconds, vendor, model and
boot_reason, preserving resource topology, capabilities and transactions. Heartbeat and unsolicited
status require accepted registration on an admitted OCPP 2.0.1 connection. Pending/rejected responses
supply the retry interval; the charger owns retry timing. Triggered exchanges while Pending remain
part of the remote-trigger workflow. Authentication alone never implies Accepted registration.

StatusNotification addresses an existing positive EVSE/connector pair. Zero, negative and unknown
addresses fail explicitly; no resource is inferred. All five native statuses are retained in
`ocpp201/evse-N/connector-N/status/status`. Reserved and Occupied project to canonical Occupied;
Available, Unavailable and Faulted map directly. Occupied does not prove physical charging or a
command's observed effect. OCPP 1.6 error fields are not accepted or invented in this message.

The schema-required timestamp remains source_time, separately from server observed_at. Older source
times are acknowledged without rewinding resource state; activity still advances. Repeated reports
replace one point slot. Unknown ordinary fields, null optional fields, invalid enums, oversized text
and malformed nested fields fail validation. Text limits count Unicode characters and permit empty
strings where the pinned schema does. Schema-valid customData requires vendorId; extension contents
are inert, discarded by this mapping, and cannot become core status or diagnostic fields.

After restart, restore the committed snapshot and bind connectivity to a newly authenticated
transport. Persisted registration alone is not live connectivity. Reconnect can resume heartbeats
without inventing a reboot; a new BootNotification reapplies current policy. All station writes must
share the same ordered owner, including metering and transaction processing. This adapter entry point
follows the existing 1.6 composition boundary; daemon-wide charging composition remains separate.

## Independent evidence

The StatusNotificationRequest schema is unmodified from the OCA 2.0.1 Edition 4 archive pinned in
`tests/ocpp-fixtures/corpus/provenance.json`. The schema and independently authored wire fixture have
SHA-256 entries in fixtures.json. Coverage rows for `ocpp201.registration.boot`, `ocpp201.heartbeat`
and `ocpp201.status` link to behavioral tests, including persisted SQLite state, close/reopen
recovery, distinct EVSEs sharing connector numbers, every status, stale observations, failed writes,
and authenticated WebSocket replies delayed until application commit. This is not certification.

```text
cargo test --locked -p uob-protocol-adapter --test ocpp201_registration --test ocpp16_registration
./scripts/verify-workspace.sh
```
