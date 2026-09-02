# Offline command rejection and uncertain recovery

The application-owned `CommandCoordinator` is the single command path used by management,
targets, and verified providers. Before durable admission or protocol dispatch, it reads the
currently admitted station session and rejects a missing, connecting, or disconnected station
with `station_disconnected`. Rejected offline requests are not stored in a dispatch queue, so a
later station connection cannot execute them. A caller may explicitly submit a new request after
connectivity is restored.

## One live dispatch attempt

For a connected station, the coordinator validates expiry and advertised capability, then stores
the command and its `admitted` lifecycle atomically. Only a newly admitted request advances to
`dispatched`; an identical retry returns the latest durable result and never calls the station
port again.

The station port must classify an attempt as one of three exact outcomes:

- `NotTransmitted` means the adapter proved no command bytes were sent, including a connection
  disappearing before the socket accepted the request.
- `ProtocolResponse` records the correlated charger response without claiming that the requested
  physical effect occurred.
- `TransmissionUncertain` means bytes may have reached the charger but no correlated response was
  established. The coordinator persists this unresolved outcome and does not retry it.

An adapter failure after possible transmission must use `TransmissionUncertain`, not a generic
unavailable error. The station port must not retain failed work for a future connection.

## Restart and observed-state reconciliation

SQLite recovery returns only commands whose latest lifecycle remains unresolved. A persisted
`dispatched` command found after restart is conservatively changed to `transmission_uncertain`,
because the new process cannot know whether the charger acted before the response was persisted.
Recovery never submits these commands to the live station port.

Later station or transaction observations are linked through `ObservedCommandEffect` records.
Adding evidence preserves the existing lifecycle: in particular, an uncertain transmission does
not become a manufactured charger acknowledgement or protocol success. Event IDs make repeated
reconciliation idempotent, while the command remains retained until an explicit future workflow
resolves its outcome.
