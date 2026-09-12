use super::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;

#[tokio::test]
async fn advertised_constraints_and_large_native_connectors_are_preserved() {
    let mut running = session("ocpp1.6", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    snapshot.resources[0].resource.native_protocol_reference =
        Some(NativeProtocolReference::Ocpp16 { connector_id: 42 });
    let reset = snapshot
        .capabilities
        .operations
        .iter_mut()
        .find(|op| matches!(op.operation, Operation::ProtocolAction { .. }))
        .unwrap();
    reset.parameters.push(OperationParameter {
        name: ParameterName::new("type").unwrap(),
        value_type: ValueType::NamedEnum,
        required: true,
        constraints: DataPointConstraints {
            minimum: None,
            maximum: None,
            enum_values: vec![NamedEnumValue::new("Soft").unwrap()],
        },
    });
    port.update_committed(snapshot.clone()).unwrap();
    let hard = command(
        &snapshot,
        "hard-denied",
        protocol("Reset", json!({"type":"Hard"})),
    );
    assert!(
        matches!(commands.submit(hard).await.unwrap().lifecycle,CommandLifecycle::Rejected {error} if error.code==CommandErrorCode::InvalidParameters)
    );
    let unlock = command(
        &snapshot,
        "large-connector",
        protocol("UnlockConnector", json!({"connectorId":42})),
    );
    let submit = {
        let commands = commands.clone();
        tokio::spawn(async move { commands.submit(unlock).await.unwrap() })
    };
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2,"large-connector","UnlockConnector",{"connectorId":42}])
    );
    running
        .peer
        .send_text(json!([3,"large-connector",{"status":"Unlocked"}]).to_string())
        .await
        .unwrap();
    accepted(&submit.await.unwrap());
    let unlock = snapshot.resources[0]
        .capabilities
        .operations
        .iter_mut()
        .find(|op| matches!(op.operation, Operation::ProtocolAction { .. }))
        .unwrap();
    unlock.parameters.push(OperationParameter {
        name: ParameterName::new("connectorId").unwrap(),
        value_type: ValueType::UnsignedInteger,
        required: true,
        constraints: DataPointConstraints {
            minimum: Some(TypedValue::UnsignedInteger(1)),
            maximum: Some(TypedValue::UnsignedInteger(41)),
            enum_values: vec![],
        },
    });
    port.update_committed(snapshot.clone()).unwrap();
    let denied = command(
        &snapshot,
        "bound",
        protocol("UnlockConnector", json!({"connectorId":42})),
    );
    assert!(
        matches!(commands.submit(denied).await.unwrap().lifecycle,CommandLifecycle::Rejected {error} if error.code==CommandErrorCode::InvalidParameters)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
