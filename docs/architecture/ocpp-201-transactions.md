# OCPP 2.0.1 transaction lifecycle

OCPP 2.0.1 `TransactionEvent` messages map to one application-owned lifecycle observation. The
mapping retains the native transaction identifier, EVSE and connector, event kind, sequence
number, trigger reason, charging state, stop reason, and source timestamp. Meter values carried by
an update remain attached to the same observation rather than hiding the lifecycle transition.

The application reconciles events against the canonical station snapshot. A start creates the
transaction, consecutive updates change its observed state, and an end records the terminal time.
The snapshot also stores the latest protocol sequence, event, trigger, resource, and source time.
This recovery evidence survives a process restart and lets an exact replay remain idempotent while
rejecting sequence gaps, stale events, changed payloads at an existing sequence, unknown EVSEs,
and events after a terminal end.

Callers commit the resulting station snapshot together with its journal event and required target
deliveries through `AtomicStoreWrite`. Because the SQLite adapter applies that write in one
database transaction, externally visible lifecycle evidence cannot precede authoritative state.
Protocol acceptance is not presented as proof of charging; the canonical state reflects the
station's reported charging state and remains distinct from authorization or command outcomes.
