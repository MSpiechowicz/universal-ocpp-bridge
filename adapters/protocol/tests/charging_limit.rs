mod endpoint_support;
#[allow(dead_code)] // Shared remote-control harness helpers are used by other test crates.
#[path = "ocpp16_remote_control/support.rs"]
mod support16;
#[allow(dead_code)] // Shared remote-control harness helpers are used by other test crates.
#[path = "ocpp201_remote_control/support.rs"]
mod support201;

use serde_json::{Value, json};
use std::time::Duration;
use uob_application::{CommandClock, CommandDispatchOutcome, StationCommandPort};
use uob_contracts::{
    ChargingLimit, CommandErrorCode, CommandOperation, EngineeringUnit, ExactDecimal,
    TransactionSnapshot,
};

fn limit(value: &str, unit: EngineeringUnit, phases: Option<u8>) -> CommandOperation<Value> {
    CommandOperation::SetChargingLimit(ChargingLimit {
        value: value.parse::<ExactDecimal>().unwrap(),
        unit,
        phases,
    })
}

#[tokio::test]
async fn ocpp16_tx_profile_targets_pending_or_active_native_transaction_without_claiming_charging()
{
    let mut running = support16::session("ocpp1.6", Duration::from_secs(2)).await;
    let db = support16::Database::new();
    let store = db.open();
    let (mut snapshot, _, port, _) = support16::setup(&store, running.handle.clone()).await;
    let resource = snapshot.resources[0].resource.clone();
    snapshot.transactions.push(serde_json::from_value(json!({
        "transaction_id":"tx-active", "resource":resource, "state":"active",
        "started_at":"2026-09-01T01:00:00Z",
        "ocpp16": {"transaction_id": 81, "start_message_id":"start-81", "start_fingerprint":"fingerprint",
            "authorization_status":"Accepted", "authorization_expiry":null,"identity_reference":null,
            "meter_start":0,"reservation_id":null,"stop_message_id":null,"stop_fingerprint":null,
            "stop_identity_fingerprint":null,"meter_stop":null,"stop_reason":null}
    })).unwrap());
    port.update_committed(snapshot.clone()).unwrap();
    assert_ocpp16_active_and_pending(&mut running, &port, &mut snapshot).await;
    assert_ocpp16_rejections(&mut running, &port, &mut snapshot).await;
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

async fn assert_ocpp16_active_and_pending(
    running: &mut support16::RunningSession,
    port: &std::sync::Arc<uob_protocol_adapter::v16::remote_control::RemoteControlSession>,
    snapshot: &mut uob_contracts::StationSnapshot,
) {
    let command = support16::command(
        snapshot,
        "limit-16",
        limit("7500", EngineeringUnit::Milliampere, Some(1)),
    )
    .admit(support16::Clock.now());
    let dispatched = {
        let port = port.clone();
        tokio::spawn(async move { port.dispatch(command).await.unwrap() })
    };
    let frame = support16::receive_json(&mut running.peer).await;
    assert_eq!(frame[2], "SetChargingProfile");
    assert_eq!(frame[3]["connectorId"], 1);
    assert_eq!(frame[3]["csChargingProfiles"]["transactionId"], 81);
    assert_eq!(
        frame[3]["csChargingProfiles"]["chargingProfilePurpose"],
        "TxProfile"
    );
    assert_eq!(
        frame[3]["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"],
        7.5
    );
    assert_eq!(
        frame[3]["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["numberPhases"],
        1
    );
    running
        .peer
        .send_text(json!([3,"limit-16",{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    assert!(matches!(
        dispatched.await.unwrap(),
        CommandDispatchOutcome::ProtocolResponse { accepted: true, .. }
    ));
    snapshot.transactions[0].state = uob_contracts::TransactionState::Pending;
    port.update_committed(snapshot.clone()).unwrap();
    let pending = support16::command(
        snapshot,
        "limit-16-pending",
        limit("7", EngineeringUnit::Ampere, None),
    )
    .admit(support16::Clock.now());
    let dispatched = {
        let port = port.clone();
        tokio::spawn(async move { port.dispatch(pending).await.unwrap() })
    };
    let frame = support16::receive_json(&mut running.peer).await;
    assert_eq!(frame[2], "SetChargingProfile");
    assert_eq!(frame[3]["connectorId"], 1);
    assert_eq!(
        frame[3]["csChargingProfiles"]["chargingProfilePurpose"],
        "TxProfile"
    );
    assert_eq!(frame[3]["csChargingProfiles"]["transactionId"], 81);
    assert_eq!(
        frame[3]["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"],
        7.0
    );
    running
        .peer
        .send_text(json!([3,"limit-16-pending",{"status":"Rejected"}]).to_string())
        .await
        .unwrap();
    assert!(matches!(
        dispatched.await.unwrap(),
        CommandDispatchOutcome::ProtocolResponse {
            accepted: false,
            ..
        }
    ));
}

async fn assert_ocpp16_rejections(
    running: &mut support16::RunningSession,
    port: &std::sync::Arc<uob_protocol_adapter::v16::remote_control::RemoteControlSession>,
    snapshot: &mut uob_contracts::StationSnapshot,
) {
    let unsupported = support16::command(
        snapshot,
        "limit-16-invalid",
        limit("7501", EngineeringUnit::Milliampere, None),
    )
    .admit(support16::Clock.now());
    assert!(
        matches!(port.dispatch(unsupported).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    let evidence = snapshot.transactions[0].ocpp16.take();
    port.update_committed(snapshot.clone()).unwrap();
    let missing = support16::command(
        snapshot,
        "limit-16-missing",
        limit("7", EngineeringUnit::Ampere, None),
    )
    .admit(support16::Clock.now());
    assert!(
        matches!(port.dispatch(missing).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    snapshot.transactions[0].ocpp16 = evidence;
    snapshot.transactions[0].state = uob_contracts::TransactionState::Suspended;
    port.update_committed(snapshot.clone()).unwrap();
    let suspended = support16::command(
        snapshot,
        "limit-16-suspended",
        limit("7", EngineeringUnit::Ampere, None),
    )
    .admit(support16::Clock.now());
    assert!(
        matches!(port.dispatch(suspended).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    snapshot.transactions[0].state = uob_contracts::TransactionState::Ended;
    port.update_committed(snapshot.clone()).unwrap();
    let ended = support16::command(
        snapshot,
        "limit-16-ended",
        limit("7", EngineeringUnit::Ampere, None),
    )
    .admit(support16::Clock.now());
    assert!(
        matches!(port.dispatch(ended).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    snapshot.transactions[0].state = uob_contracts::TransactionState::Uncertain;
    port.update_committed(snapshot.clone()).unwrap();
    let stale = support16::command(
        snapshot,
        "limit-16-stale",
        limit("7", EngineeringUnit::Ampere, None),
    )
    .admit(support16::Clock.now());
    assert!(
        matches!(port.dispatch(stale).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ocpp201_tx_profile_targets_pending_or_active_native_transaction_and_rejects_ambiguous_evse()
 {
    let mut running = support201::session("ocpp2.0.1", Duration::from_secs(2)).await;
    let db = support201::Database::new();
    let store = db.open();
    let (mut snapshot, _, port, _) = support201::setup(&store, running.handle.clone()).await;
    let resource = snapshot.resources[0].resource.clone();
    let transaction: TransactionSnapshot = serde_json::from_value(json!({
        "transaction_id":"tx-active", "resource":resource, "state":"active",
        "started_at":"2026-09-01T01:00:00Z",
        "protocol_state": {"protocol":"ocpp201","native_transaction_id":"native-82",
            "native_resource":{"protocol":"ocpp201","evse_id":1,"connector_id":1},
            "last_sequence_number":1,"last_event":"Updated","last_trigger_reason":"MeterValuePeriodic",
            "last_event_at":"2026-09-01T01:00:00Z","last_event_fingerprint":"fingerprint"}
    })).unwrap();
    snapshot.transactions.push(transaction);
    snapshot.resources[0]
        .capabilities
        .operations
        .push(uob_contracts::SupportedOperation {
            operation: uob_contracts::Operation::SetChargingLimit,
            parameters: vec![],
        });
    port.update_committed(snapshot.clone()).unwrap();
    assert_ocpp201_active_and_pending(&mut running, &port, &mut snapshot).await;
    assert_ocpp201_invalid_and_ambiguous(&mut running, &port, &mut snapshot).await;
    assert_ocpp201_terminal(&mut running, &port, &mut snapshot).await;
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

async fn assert_ocpp201_active_and_pending(
    running: &mut support201::RunningSession,
    port: &std::sync::Arc<uob_protocol_adapter::v201::remote_control::RemoteControlSession>,
    snapshot: &mut uob_contracts::StationSnapshot,
) {
    let command = support201::command(
        snapshot,
        "limit-201",
        limit("7.2", EngineeringUnit::Kilowatt, Some(3)),
    )
    .admit(support201::Clock.now());
    let dispatched = {
        let port = port.clone();
        tokio::spawn(async move { port.dispatch(command).await.unwrap() })
    };
    let frame = support201::receive_json(&mut running.peer).await;
    assert_eq!(frame[2], "SetChargingProfile");
    assert_eq!(frame[3]["evseId"], 1);
    assert_eq!(
        frame[3]["chargingProfile"]["chargingProfilePurpose"],
        "TxProfile"
    );
    assert_eq!(frame[3]["chargingProfile"]["transactionId"], "native-82");
    assert_eq!(
        frame[3]["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"],
        7200
    );
    running
        .peer
        .send_text(json!([3,"limit-201",{"status":"Rejected"}]).to_string())
        .await
        .unwrap();
    assert!(matches!(
        dispatched.await.unwrap(),
        CommandDispatchOutcome::ProtocolResponse {
            accepted: false,
            ..
        }
    ));
    snapshot.transactions[0].state = uob_contracts::TransactionState::Pending;
    port.update_committed(snapshot.clone()).unwrap();
    let pending = support201::command(
        snapshot,
        "limit-201-pending",
        limit("7.2", EngineeringUnit::Kilowatt, Some(3)),
    )
    .admit(support201::Clock.now());
    let dispatched = {
        let port = port.clone();
        tokio::spawn(async move { port.dispatch(pending).await.unwrap() })
    };
    let frame = support201::receive_json(&mut running.peer).await;
    assert_eq!(frame[2], "SetChargingProfile");
    assert_eq!(frame[3]["evseId"], 1);
    assert_eq!(
        frame[3]["chargingProfile"]["chargingProfilePurpose"],
        "TxProfile"
    );
    assert_eq!(frame[3]["chargingProfile"]["transactionId"], "native-82");
    assert_eq!(
        frame[3]["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"],
        7200
    );
    running
        .peer
        .send_text(json!([3,"limit-201-pending",{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    assert!(matches!(
        dispatched.await.unwrap(),
        CommandDispatchOutcome::ProtocolResponse { accepted: true, .. }
    ));
}

async fn assert_ocpp201_invalid_and_ambiguous(
    running: &mut support201::RunningSession,
    port: &std::sync::Arc<uob_protocol_adapter::v201::remote_control::RemoteControlSession>,
    snapshot: &mut uob_contracts::StationSnapshot,
) {
    let unsupported = support201::command(
        snapshot,
        "limit-201-invalid",
        limit("7200.01", EngineeringUnit::Watt, None),
    )
    .admit(support201::Clock.now());
    assert!(
        matches!(port.dispatch(unsupported).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    let mut second = snapshot.transactions[0].clone();
    second.transaction_id = serde_json::from_value(json!("tx-second")).unwrap();
    second.resource = snapshot.resources[1].resource.clone();
    "native-83".clone_into(
        &mut second
            .protocol_state
            .as_mut()
            .unwrap()
            .native_transaction_id,
    );
    second.protocol_state.as_mut().unwrap().native_resource =
        second.resource.native_protocol_reference.unwrap();
    snapshot.transactions.push(second);
    port.update_committed(snapshot.clone()).unwrap();
    let ambiguous = support201::command(
        snapshot,
        "limit-201-ambiguous",
        limit("7200", EngineeringUnit::Watt, None),
    )
    .admit(support201::Clock.now());
    assert!(
        matches!(port.dispatch(ambiguous).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    snapshot.transactions.pop();
    snapshot.transactions[0]
        .protocol_state
        .as_mut()
        .unwrap()
        .native_transaction_id
        .clear();
    port.update_committed(snapshot.clone()).unwrap();
    let missing = support201::command(
        snapshot,
        "limit-201-missing",
        limit("7200", EngineeringUnit::Watt, None),
    )
    .admit(support201::Clock.now());
    assert!(
        matches!(port.dispatch(missing).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
}

async fn assert_ocpp201_terminal(
    running: &mut support201::RunningSession,
    port: &std::sync::Arc<uob_protocol_adapter::v201::remote_control::RemoteControlSession>,
    snapshot: &mut uob_contracts::StationSnapshot,
) {
    "native-82".clone_into(
        &mut snapshot.transactions[0]
            .protocol_state
            .as_mut()
            .unwrap()
            .native_transaction_id,
    );
    snapshot.transactions[0].state = uob_contracts::TransactionState::Suspended;
    port.update_committed(snapshot.clone()).unwrap();
    let suspended = support201::command(
        snapshot,
        "limit-201-suspended",
        limit("7200", EngineeringUnit::Watt, None),
    )
    .admit(support201::Clock.now());
    assert!(
        matches!(port.dispatch(suspended).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    snapshot.transactions[0].state = uob_contracts::TransactionState::Ended;
    port.update_committed(snapshot.clone()).unwrap();
    let ended = support201::command(
        snapshot,
        "limit-201-ended",
        limit("7200", EngineeringUnit::Watt, None),
    )
    .admit(support201::Clock.now());
    assert!(
        matches!(port.dispatch(ended).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    snapshot.transactions[0].state = uob_contracts::TransactionState::Uncertain;
    port.update_committed(snapshot.clone()).unwrap();
    let stale = support201::command(
        snapshot,
        "limit-201-stale",
        limit("7200", EngineeringUnit::Watt, None),
    )
    .admit(support201::Clock.now());
    assert!(
        matches!(port.dispatch(stale).await.unwrap(), CommandDispatchOutcome::NotTransmitted { error, .. } if error.code == CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
}
