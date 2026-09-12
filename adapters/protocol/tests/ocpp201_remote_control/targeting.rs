use super::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::remote_control::RemoteControlStore;
use uob_application::*;
use uob_contracts::*;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the scoped wire assertions together.
async fn evse_reset_and_connector_unlock_keep_exact_scope_and_constraints() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    snapshot.resources[0]
        .capabilities
        .operations
        .push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: "Reset".to_owned(),
            },
            parameters: vec![OperationParameter {
                name: ParameterName::new("type").unwrap(),
                value_type: ValueType::NamedEnum,
                required: true,
                constraints: DataPointConstraints {
                    minimum: None,
                    maximum: None,
                    enum_values: vec![NamedEnumValue::new("OnIdle").unwrap()],
                },
            }],
        });
    port.update_committed(snapshot.clone()).unwrap();
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    for (id, resource, operation) in [
        (
            "widen-reset",
            snapshot.resources[0].resource.clone(),
            protocol("Reset", json!({"type":"OnIdle"})),
        ),
        (
            "wrong-reset",
            snapshot.resources[0].resource.clone(),
            protocol("Reset", json!({"type":"OnIdle","evseId":2})),
        ),
        (
            "constraint",
            snapshot.resources[0].resource.clone(),
            protocol("Reset", json!({"type":"Immediate","evseId":1})),
        ),
        (
            "wrong-evse",
            snapshot.resources[1].resource.clone(),
            protocol("UnlockConnector", json!({"evseId":2,"connectorId":1})),
        ),
        (
            "widen-start",
            snapshot.resources[1].resource.clone(),
            CommandOperation::Start {
                authorization_reference: Some(reference().await.as_str().to_owned()),
            },
        ),
    ] {
        let mut request = command(&snapshot, id, operation);
        request.request.resource = resource;
        assert!(matches!(
            commands.submit(request).await.unwrap().lifecycle,
            CommandLifecycle::Rejected { .. }
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    let mut reset = command(
        &snapshot,
        "reset-evse",
        protocol("Reset", fixture("reset-evse")[3].clone()),
    );
    reset.request.resource = snapshot.resources[0].resource.clone();
    let submit = {
        let commands = commands.clone();
        tokio::spawn(async move { commands.submit(reset).await.unwrap() })
    };
    assert_eq!(receive_json(&mut running.peer).await, fixture("reset-evse"));
    running
        .peer
        .send_text(fixture("reset-evse-scheduled").to_string())
        .await
        .unwrap();
    let result = submit.await.unwrap();
    accepted(&result);
    assert!(result.observed_effects.is_empty());
    assert_eq!(
        store
            .remote_control_evidence(RequestId::new("reset-evse").unwrap())
            .await
            .unwrap()
            .unwrap()
            .response_status
            .as_deref(),
        Some("Scheduled")
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn station_start_lets_charger_select_evse_without_reusing_other_start_identity() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, auth, port, coordinator) = setup(&store, running.handle.clone()).await;
    snapshot.capabilities.operations.push(SupportedOperation {
        operation: Operation::Start,
        parameters: vec![],
    });
    auth.apply_change(AuthorizationChange {
        reference: reference().await,
        resource: snapshot.station.clone(),
        state: AuthorizationState::Active,
        revision: 2,
        changed_at: Clock.now(),
        expires_at: None,
    })
    .await
    .unwrap();
    port.update_committed(snapshot.clone()).unwrap();
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![AccessPermission::Control],
    ));
    for (name, resource, expected_id) in [
        ("evse", snapshot.resources[0].resource.clone(), 1),
        ("station", snapshot.station.clone(), 2),
    ] {
        let mut start = command(
            &snapshot,
            name,
            CommandOperation::Start {
                authorization_reference: Some(reference().await.as_str().to_owned()),
            },
        );
        start.request.resource = resource;
        let submit = {
            let commands = commands.clone();
            tokio::spawn(async move { commands.submit(start).await.unwrap() })
        };
        let mut expected = fixture("remote-start");
        expected[1] = json!(name);
        expected[3]["remoteStartId"] = json!(expected_id);
        if name == "station" {
            expected[3].as_object_mut().unwrap().remove("evseId");
        }
        assert_eq!(receive_json(&mut running.peer).await, expected);
        running
            .peer
            .send_text(json!([3,name,{"status":"Accepted"}]).to_string())
            .await
            .unwrap();
        accepted(&submit.await.unwrap());
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
