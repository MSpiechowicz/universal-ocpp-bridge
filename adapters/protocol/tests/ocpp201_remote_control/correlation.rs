use super::support::{fixture, time};
use serde_json::{Value, json};
use uob_application::{ChargerObservation, TransactionApplyError, apply_transaction_event};
use uob_contracts::*;
use uob_protocol_adapter::v201::{self, remote_control::observation::transaction_effect};

#[test]
fn updated_transaction_correlates_once_and_conflicting_ids_do_not_mutate_state() {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    let mut frame = fixture("transaction-started");
    frame[3]["timestamp"] = json!("2026-09-01T02:00:00Z");
    let ChargerObservation::TransactionEvent(started) =
        v201::decode_call(frame.to_string().as_bytes())
            .unwrap()
            .observation
    else {
        panic!("start")
    };
    apply_transaction_event(&mut snapshot, &started, started.occurred_at).unwrap();
    let mut frame = fixture("transaction-updated");
    frame[3]["timestamp"] = json!("2026-09-01T02:02:00Z");
    frame[3]["triggerReason"] = json!("RemoteStart");
    frame[3]["transactionInfo"]["remoteStartId"] = json!(7);
    let ChargerObservation::TransactionEvent(updated) =
        v201::decode_call(frame.to_string().as_bytes())
            .unwrap()
            .observation
    else {
        panic!("update")
    };
    apply_transaction_event(&mut snapshot, &updated, updated.occurred_at).unwrap();
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.resource = snapshot.resources[0].resource.clone();
    command.admitted_at = time("2026-09-01T02:01:00Z");
    let event: EventEnvelope<TransactionSnapshot> = serde_json::from_value(json!({
        "event_id":"updated-event","schema_version":{"major":1,"revision":0},
        "runtime":{"environment":"demo","release_id":"test","release_digest":"sha256:test","process_instance_id":"test"},
        "resource":command.resource,"observed_at":updated.occurred_at,"event_type":"transaction.updated",
        "origin":{"kind":"station"},"sequence":1,"payload":snapshot.transactions[0]
    })).unwrap();
    let evidence = RemoteControlEvidence {
        remote_start_id: Some(7),
        response_status: Some("Accepted".to_owned()),
        native_transaction_id: Some("independent-transaction-001".to_owned()),
    };
    assert!(transaction_effect(&command, &event, &evidence).is_some());
    let unchanged = snapshot.clone();
    let mut conflicting = updated.clone();
    conflicting.sequence_number = 2;
    conflicting.remote_start_id = Some(8);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &conflicting, updated.occurred_at),
        Err(TransactionApplyError::ConflictingReplay)
    );
    assert_eq!(snapshot, unchanged);
    let mut later = updated;
    later.sequence_number = 2;
    later.remote_start_id = None;
    apply_transaction_event(&mut snapshot, &later, later.occurred_at).unwrap();
    assert_eq!(
        snapshot.transactions[0]
            .protocol_state
            .as_ref()
            .unwrap()
            .remote_start_id,
        Some(7)
    );
    frame[3]["transactionInfo"]["remoteStartId"] = json!(-1);
    assert!(v201::decode_call(frame.to_string().as_bytes()).is_err());
}
