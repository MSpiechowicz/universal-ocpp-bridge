use super::support::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::{self, remote_control::RemoteControlSession};

#[path = "trigger/preacceptance.rs"]
mod preacceptance;

fn enable(snapshot: &mut StationSnapshot, port: &Arc<RemoteControlSession>) {
    for capabilities in std::iter::once(&mut snapshot.capabilities).chain(
        snapshot
            .resources
            .iter_mut()
            .map(|entry| &mut entry.capabilities),
    ) {
        capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: "TriggerMessage".to_owned(),
            },
            parameters: vec![],
        });
    }
    port.update_committed(snapshot.clone()).unwrap();
}

fn request(
    snapshot: &StationSnapshot,
    id: &str,
    payload: Value,
    station: bool,
) -> ExternalCommand<Value> {
    let mut external = command(snapshot, id, protocol("TriggerMessage", payload));
    if station {
        external.request.resource = snapshot.station.clone();
    }
    external
}

fn controls(
    coordinator: Arc<Coordinator>,
    snapshot: &StationSnapshot,
) -> Arc<ScopedCommandAdmissionPort<Value>> {
    Arc::new(scoped(
        coordinator,
        snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ))
}

fn expected_frame(name: &str, id: &str) -> Value {
    let mut frame = fixture(name);
    frame[1] = json!(id);
    frame
}

async fn assert_trigger_case(
    running: &mut RunningSession,
    store: &Store,
    snapshot: &StationSnapshot,
    commands: &Arc<ScopedCommandAdmissionPort<Value>>,
    case: (&str, &str, Option<u32>, bool, &str, Vec<u32>),
) {
    let (name, class, scope, station, status, expected_targets) = case;
    let mut payload = json!({"requestedMessage": class});
    if let Some(id) = scope {
        payload["connectorId"] = json!(id);
    }
    let external = request(snapshot, name, payload, station);
    let sender = commands.clone();
    let submission = tokio::spawn(async move { sender.submit(external).await.unwrap() });
    assert_eq!(receive_json(&mut running.peer).await, fixture(name));
    let dispatched = store
        .command_result_by_request_id(RequestId::new(name).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dispatched.lifecycle, CommandLifecycle::Dispatched);
    let observation = dispatched.trigger_observation.unwrap();
    assert_eq!(observation.native_scope, scope);
    assert_eq!(observation.expected_targets, expected_targets);
    assert_eq!(observation.native_response, None);
    let response = expected_frame(&format!("trigger-{status}"), name);
    running.peer.send_text(response.to_string()).await.unwrap();
    let result = submission.await.unwrap();
    let observation = result.trigger_observation.as_ref().unwrap();
    let native = match status {
        "accepted" => TriggerNativeResponse::Accepted,
        "rejected" => TriggerNativeResponse::Rejected,
        _ => TriggerNativeResponse::NotImplemented,
    };
    assert_eq!(observation.native_response, Some(native));
    assert_eq!(observation.expected_targets, expected_targets);
    assert!(observation.observed.is_empty());
    assert_eq!(
        observation.status,
        if status == "accepted" {
            TriggerObservationStatus::Pending
        } else {
            TriggerObservationStatus::Unsupported
        }
    );
    assert!(
        matches!(&result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. } if *accepted == (status == "accepted"))
    );
    assert_eq!(
        store
            .command_result_by_request_id(RequestId::new(name).unwrap())
            .await
            .unwrap(),
        Some(result)
    );
}

#[tokio::test]
async fn trigger_six_classes_and_three_native_responses_keep_independent_durable_status() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port);
    let commands = controls(coordinator, &snapshot);
    for (name, class, scope, station, status, expected_targets) in [
        (
            "trigger-diagnostics",
            "DiagnosticsStatusNotification",
            None,
            true,
            "not-implemented",
            vec![0],
        ),
        (
            "trigger-firmware",
            "FirmwareStatusNotification",
            None,
            true,
            "rejected",
            vec![0],
        ),
        (
            "trigger-heartbeat-irrelevant-connector",
            "Heartbeat",
            Some(2),
            true,
            "accepted",
            vec![0],
        ),
        (
            "trigger-meter-connector-1",
            "MeterValues",
            Some(1),
            false,
            "accepted",
            vec![1],
        ),
        (
            "trigger-status-station",
            "StatusNotification",
            Some(0),
            true,
            "accepted",
            vec![0],
        ),
        (
            "trigger-status-all",
            "StatusNotification",
            None,
            true,
            "accepted",
            vec![0, 1, 2],
        ),
    ] {
        assert_trigger_case(
            &mut running,
            &store,
            &snapshot,
            &commands,
            (name, class, scope, station, status, expected_targets),
        )
        .await;
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn invalid_scope_capability_registration_and_privilege_never_send_trigger() {
    let mut running = session("ocpp1.6", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    let commands = controls(coordinator.clone(), &snapshot);
    let missing_capability = request(
        &snapshot,
        "missing-trigger-capability",
        json!({"requestedMessage":"Heartbeat"}),
        true,
    );
    assert!(
        matches!(commands.submit(missing_capability).await.unwrap().lifecycle,
        CommandLifecycle::Rejected { error } if error.code == CommandErrorCode::UnsupportedOperation)
    );
    enable(&mut snapshot, &port);
    let commands = controls(coordinator.clone(), &snapshot);
    let denied = scoped(coordinator, &snapshot, vec![AccessPermission::Control]);
    assert!(
        denied
            .submit(request(
                &snapshot,
                "trigger-no-grant",
                json!({"requestedMessage":"Heartbeat"}),
                true
            ))
            .await
            .is_err()
    );
    let cases = [
        (
            "trigger-unknown",
            json!({"requestedMessage":"StartTransaction"}),
            true,
        ),
        (
            "trigger-extra",
            json!({"requestedMessage":"Heartbeat","extra":true}),
            true,
        ),
        (
            "trigger-meter-zero",
            json!({"requestedMessage":"MeterValues","connectorId":0}),
            true,
        ),
        (
            "trigger-spoofed",
            json!({"requestedMessage":"StatusNotification","connectorId":2}),
            false,
        ),
        (
            "trigger-broad-connector",
            json!({"requestedMessage":"StatusNotification"}),
            false,
        ),
        (
            "trigger-after-boot",
            json!({"requestedMessage":"BootNotification"}),
            true,
        ),
    ];
    for (id, payload, station) in cases {
        let result = commands
            .submit(request(&snapshot, id, payload, station))
            .await
            .unwrap();
        assert!(
            matches!(result.lifecycle, CommandLifecycle::Rejected { .. }),
            "{id}"
        );
        assert!(result.trigger_observation.is_none(), "{id}");
    }
    let mut pending = snapshot.clone();
    for value in &mut pending.current_values {
        if value.point_id.as_str() == "ocpp16/registration/status" {
            value.value = Some(TypedValue::Text("Pending".to_owned()));
        }
    }
    port.update_committed(pending.clone()).unwrap();
    let non_boot = commands
        .submit(request(
            &pending,
            "trigger-pending-heartbeat",
            json!({"requestedMessage":"Heartbeat"}),
            true,
        ))
        .await
        .unwrap();
    assert!(matches!(
        non_boot.lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn delayed_native_reply_and_disconnect_preserve_uncertainty_without_replay() {
    let mut running = session("ocpp1.6", Duration::from_millis(150)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port);
    let commands = controls(coordinator, &snapshot);
    let slow = request(
        &snapshot,
        "trigger-slow",
        json!({"requestedMessage":"StatusNotification","connectorId":1}),
        false,
    );
    let sender = commands.clone();
    let sent = slow.clone();
    let submission = tokio::spawn(async move { sender.submit(sent).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        expected_frame("trigger-status-connector-1", "trigger-slow")
    );
    let result = submission.await.unwrap();
    assert!(matches!(
        &result.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert_eq!(
        result.trigger_observation.as_ref().unwrap().native_response,
        None
    );
    running
        .peer
        .send_text(expected_frame("trigger-accepted", "trigger-slow").to_string())
        .await
        .unwrap();
    assert_eq!(commands.submit(slow).await.unwrap(), result);
    let disconnect = request(
        &snapshot,
        "trigger-disconnect",
        json!({"requestedMessage":"MeterValues","connectorId":1}),
        false,
    );
    let sender = commands.clone();
    let sent = disconnect.clone();
    let submission = tokio::spawn(async move { sender.submit(sent).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        expected_frame("trigger-meter-connector-1", "trigger-disconnect")
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    let uncertain = submission.await.unwrap();
    assert!(!matches!(
        &uncertain.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
    assert_eq!(
        uncertain
            .trigger_observation
            .as_ref()
            .unwrap()
            .native_response,
        None
    );
    assert_eq!(commands.submit(disconnect).await.unwrap(), uncertain);
    let reopened = database.open();
    for (id, expected) in [("trigger-slow", result), ("trigger-disconnect", uncertain)] {
        assert_eq!(
            reopened
                .command_result_by_request_id(RequestId::new(id).unwrap())
                .await
                .unwrap(),
            Some(expected)
        );
    }
    running.server.abort();
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn omitted_status_scope_includes_station_and_all_64_admitted_connectors() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    let connector = snapshot.resources[0].clone();
    snapshot.resources = (1..=64)
        .map(|id| {
            let mut entry = connector.clone();
            entry.resource.resource = Some(CanonicalResource::Connector {
                connector_id: CanonicalConnectorId::new(format!("connector-{id}")).unwrap(),
            });
            entry.resource.native_protocol_reference =
                Some(NativeProtocolReference::Ocpp16 { connector_id: id });
            entry
        })
        .collect();
    enable(&mut snapshot, &port);
    let commands = controls(coordinator, &snapshot);
    let external = request(
        &snapshot,
        "trigger-status-64",
        json!({"requestedMessage":"StatusNotification"}),
        true,
    );
    let submission = tokio::spawn(async move { commands.submit(external).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        expected_frame("trigger-status-all", "trigger-status-64")
    );
    let dispatched = store
        .command_result_by_request_id(RequestId::new("trigger-status-64").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        dispatched.trigger_observation.unwrap().expected_targets,
        (0..=64).collect::<Vec<_>>()
    );
    running
        .peer
        .send_text(expected_frame("trigger-accepted", "trigger-status-64").to_string())
        .await
        .unwrap();
    let result = submission.await.unwrap();
    assert_eq!(
        result.trigger_observation.unwrap().expected_targets,
        (0..=64).collect::<Vec<_>>()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
