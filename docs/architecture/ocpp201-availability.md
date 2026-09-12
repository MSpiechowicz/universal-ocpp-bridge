# OCPP 2.0.1 availability

`v201::remote_control::RemoteControlSession` dispatches `ChangeAvailability` through the existing
scoped privileged-command coordinator, resource capabilities, durable deduplication, expiry and
bounded socket lifecycle. The schema is `urn:OCPP:Cp:2:2020:3:ChangeAvailabilityRequest`.

Omitting `evse` requires a station resource. An EVSE resource requires exactly `evse.id`; a connector
requires both `evse.id` and `evse.connectorId`. IDs must be positive signed-32-bit values and match
the authenticated resource. Unknown fields, null scope, unsupported capabilities, scope widening,
invalid operational status and expired commands are rejected before transmission. The adapter
supports the standard payload; vendor extension fields are explicitly rejected.

Accepted, Scheduled and Rejected remain durable native response evidence. Accepted and Scheduled
are affirmative protocol responses, not observed effects. Neither changes a snapshot or ends a
transaction. The command retains its desired operational status and precise resource across
restart. Missing, malformed, late or interrupted responses remain uncertain; no automatic replay
occurs. Failure to persist native response evidence also leaves the command uncertain.

The ordered station owner calls `v201::availability::complete_status` for StatusNotification when
availability journal evidence is required, publishes the committed snapshot to the command port,
and acknowledges only after the snapshot and journal commit atomically. Other registration calls
continue through `complete_registration`. Existing transaction handlers own transaction completion.
Snapshots preserve each connector's native Available, Occupied, Reserved, Unavailable or Faulted
status. Occupied does not establish physical charging. Old source timestamps cannot overwrite newer
connector observations. EVSE aggregate availability alone never admits remote start: at least one
connector in the selected scope must be available and its EVSE must have no active transaction.

`observed_effect` consumes authoritative station journal events and the durable command. It requires
fresh good-quality connector evidence in the exact scope, and for Inoperative, no overlapping active
transaction. EVSE and station requests require every known connector in scope; no fictitious
station-zero status is required. The ordinary coordinator links effects idempotently without
replacing the native reply. Evidence means compatible observed state, not causation: native status
notifications contain no availability request ID.

Parent and child operational states are independent. Making an EVSE Operative does not force a
previously Inoperative or Faulted connector to become Available. The bridge preserves these native
reports. Where connector-only evidence cannot establish the complete requested parent state, it
conservatively leaves the effect unresolved. It does not synthesize parent state, restore child
state, or misinterpret a fault as a successful command. Device-model NotifyEvent monitoring belongs
to the separate monitoring feature; this path uses StatusNotification.

## Specification and behavioral evidence

The independent corpus pins OCPP 2.0.1 Edition 4 plus June 2026 errata. ChangeAvailability request
and response schemas are copied unchanged from the checksum-verified official schema bundle.
Hand-authored wire fixtures are independent of the protocol model serialization. The following
CSMS-side assertions map the availability requirements; charger firmware behavior is observed,
not implemented by the bridge and not claimed as certified.

| Requirements | Evidence in `ocpp201_availability` |
| --- | --- |
| G03.FR.01–03, G04.FR.02–04: native replies, including already-set state | `wire::native_responses_persist_without_changing_observed_availability` |
| G03.FR.04, G04.FR.05: connector observations separate from replies | `wire::delayed_reply_keeps_status_processing_live_and_operative_evidence_separate`, `rejections::status_failure_is_atomic_and_source_time_cannot_confirm_an_old_observation` |
| G03.FR.05, G04.FR.06: scheduled transaction completion | `wire::scheduled_transaction_waits_for_completion_and_fresh_status_evidence` |
| G03.FR.06–07,09, G04.FR.07–08: individual connector states and EVSE boundaries | `scopes::evse_scope_preserves_connector_states_and_never_controls_another_evse`, `failures::connector_unavailability_blocks_evse_start_until_observed_operative` |
| G04.FR.01: station-wide scope | `wire::station_scope_waits_for_every_connector_and_never_widens_connector_control` |
| G03.FR.08, G04.FR.09: reboot persistence | `recovery::restart_retains_scheduled_intent_and_reconciles_without_replay` |
| Invalid, unauthorized and unsupported scope | `rejections::invalid_unsupported_unprivileged_and_expired_requests_never_reach_wire`, `scopes::nested_scope_null_zero_unknown_and_cross_evse_requests_are_rejected` |
| Missing replies, crashes, storage failure | `recovery::missing_malformed_late_and_callerror_responses_never_invent_availability`, `failures::native_evidence_failure_keeps_the_dispatched_command_uncertain` |

Run `cargo test --locked -p uob-protocol-adapter --test ocpp201_availability` and
`cargo run --locked --quiet -p uob-ocpp-fixtures` for focused evidence. The full workspace verifier
also runs these tests, architecture boundaries, documentation and file-size checks.
