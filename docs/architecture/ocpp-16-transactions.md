# OCPP 1.6 durable transaction lifecycle

`v16::transaction_call` and `complete_transaction` handle validated StartTransaction and
StopTransaction calls inside the authenticated station's single ordered owner. The latter
consumes the decoded call from the bounded socket receiver; its response payload is sent through
that call's existing responder. The owner restores and shares the same authoritative snapshot
with registration, status and metering, and holds ownership across asynchronous authorization
and commit. Disconnected/unregistered stations and unknown connectors cannot create transactions.

The operational store reserves positive signed 32-bit CSMS transaction IDs with a single atomic
SQLite counter update, shared across station workers and database connections. Reservations
survive restart, never wrap, and may leave gaps after failed work. The counter is an additive
internal table; canonical snapshots carry an optional `ocpp16` evidence object. No existing
snapshot fields or OCPP 2.0.1 sequencing semantics change.

Each start resolves its idTag through the existing authorization provider with a bounded deadline
and consults current durable local policy after resolution. Accepted, Blocked, Expired and Invalid
retain their protocol meaning. A previously committed retry reuses its original decision and ID,
even after later revocation. This is response recovery, not a new authorization grant. Raw idTags
remain transient and redacted in Debug; durable evidence contains only opaque identity references
and payload fingerprints. StopTransaction never depends on renewed permission to charge and
returns an empty, schema-valid response without extending the charger's authorization cache.

A start is a station-reported transaction, including when authorization is denied. It remains
canonically Pending until other observations establish charging; denial cannot erase a reported
session. Starts retain source time, connector, meterStart in Wh, reservation ID, authorization
status/expiry and retry evidence. Stops retain meterStop, source time, native reason (absence
means Local), optional stopping-identity fingerprint, and transactionData with exact meter
semantics, timestamps, quality and signed evidence. Samples are associated with the transaction's
persisted connector, since StopTransaction carries no connectorId. Register decreases are
preserved rather than silently converted to unsigned energy deltas.

The application commits the changed snapshot, a critical canonical transaction event and any
selected-target delivery in one `AtomicStoreWrite`. The host supplies the trusted event ID,
resource-stream sequence, service identity and selected target revision. No response is exposed
before commit. A failed write leaves the in-memory snapshot unchanged. Target unavailability
does not prevent a local commit; required deliveries remain in the durable outbox. Starts use
ordinary new-session admission, while stops can consume protected active-session completion space.

Exact canonical payload retries are idempotent across reconnect and process restart, including
when a charger assigns a new CALL ID. Changed reuse of retained IDs, overlapping starts on one
connector, unknown transaction IDs, stops before start time, and altered terminal reports fail
explicitly. No transaction command is replayed and no physical stop/start is generated here.

## Bounds and recovery

The existing 256 KiB OCPP frame limit applies before decoding; stop details are additionally
bounded to 256 samples. A station retains at most 128 transaction records, including other
protocol records. Full unexpired history refuses new starts while still permitting stops.
Ended OCPP 1.6 snapshot records older than seven days can be pruned during a subsequent commit;
active transactions are never pruned. A monotonic replay cutoff is persisted in the same commit,
so expired start reports cannot become new transactions even if the host clock moves backwards.
Unseen offline starts older than that retained window require operator reconciliation. Critical
events and pending target deliveries follow their own existing retention rules and are not deleted
by snapshot pruning. IDs are never reused when records expire.

The focused `ocpp16_transactions` suite exercises independent OCA-schema fixtures, committed
snapshot/event/outbox contents, failed atomic writes, denied/delayed authorization, frame/sample
bounds, registration/storage pressure, ID allocation, retention and real authenticated WebSocket
reconnects. The fixture corpus records exact request/response schemas from the checksum-pinned
OCPP 1.6 archive and links lifecycle coverage to those behavioral tests. This is feature evidence,
not an OCPP certification claim.
