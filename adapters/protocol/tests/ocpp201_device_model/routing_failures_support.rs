use crate::{endpoint_support::TEST_BOUND, support::*};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;
use uob_application::*;
use uob_contracts::*;

pub type Commands = Arc<ScopedCommandAdmissionPort<Value>>;

pub fn station_commands(coordinator: Arc<Coordinator>, snapshot: &StationSnapshot) -> Commands {
    Arc::new(scoped(
        coordinator,
        snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ))
}

pub fn report_request(
    snapshot: &StationSnapshot,
    id: &str,
    action: &str,
    payload: Value,
    resource: ResourceRef,
) -> ExternalCommand<Value> {
    let mut external = command(snapshot, id, protocol(action, payload));
    external.request.resource = resource;
    external
}

pub async fn start_report(
    running: &mut RunningSession,
    commands: &Commands,
    external: ExternalCommand<Value>,
) -> CommandResult {
    let id = external.request.request_id.as_str().to_owned();
    let CommandOperation::Ocpp(operation) = &external.request.operation else {
        panic!("native query");
    };
    let expected = json!([2, id, operation.action, operation.payload]);
    let commands = commands.clone();
    let submitted = tokio::spawn(async move { commands.submit(external).await.unwrap() });
    assert_eq!(receive_json(&mut running.peer).await, expected);
    running
        .peer
        .send_text(json!([3, id, {"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let result = timeout(TEST_BOUND, submitted).await.unwrap().unwrap();
    accepted(&result);
    let evidence = result.device_model_201.as_ref().unwrap();
    assert_eq!(evidence.native_ack, Some(DeviceReportAck201::Accepted));
    assert_eq!(evidence.report, DeviceReportState201::Pending);
    result
}

pub fn item(component: Value, variable: &str) -> Value {
    let mut result = json!({
        "variable": {"name": variable},
        "variableAttribute": [{"value":"SECRET-ROUTING-VALUE"}]
    });
    result["component"] = component;
    result
}

pub fn notification(id: &str, request: i32, sequence: u32, more: bool, items: Vec<Value>) -> Value {
    let mut payload = json!({
        "requestId": request,
        "generatedAt": "2026-09-01T02:00:01Z",
        "seqNo": sequence,
        "tbc": more
    });
    if !items.is_empty() {
        payload["reportData"] = Value::Array(items);
    }
    Value::Array(vec![json!(2), json!(id), json!("NotifyReport"), payload])
}

pub async fn notify(running: &mut RunningSession, frame: Value) {
    running.peer.send_text(frame.to_string()).await.unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([3, frame[1], {}])
    );
}

pub async fn terminal(store: &Store, id: &str) -> CommandResult {
    timeout(TEST_BOUND, async {
        loop {
            let result = result(store, id).await;
            if !result.device_model_201.as_ref().unwrap().report.pending() {
                return result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("durable report transition")
}

pub async fn result(store: &Store, id: &str) -> CommandResult {
    store
        .command_result_by_request_id(RequestId::new(id).unwrap())
        .await
        .unwrap()
        .unwrap()
}

pub fn incomplete(
    result: &CommandResult,
    expected_reason: DeviceReportFailure201,
    fragments: u32,
    items: usize,
) {
    accepted(result);
    let evidence = result.device_model_201.as_ref().unwrap();
    assert_eq!(evidence.native_ack, Some(DeviceReportAck201::Accepted));
    assert!(evidence.variables.is_empty());
    let DeviceReportState201::Incomplete { reason, progress } = &evidence.report else {
        panic!("failed collection must not publish partial inventory");
    };
    assert_eq!(*reason, expected_reason);
    let progress = progress.as_ref().expect("live collector progress");
    assert_eq!((progress.fragments, progress.items), (fragments, items));
    if fragments == 0 {
        assert_eq!(progress.bytes, 0);
    }
    let report = serde_json::to_value(&evidence.report).unwrap();
    assert!(report.get("items").is_none());
    assert!(report.get("fragments").is_none());
    assert!(
        !serde_json::to_string(result)
            .unwrap()
            .contains("SECRET-ROUTING-VALUE")
    );
}

pub async fn silence(running: &mut RunningSession) {
    assert!(
        timeout(Duration::from_millis(150), running.peer.receive())
            .await
            .is_err(),
        "denied queries, automatic retries and renumbered CALLs must not reach the station"
    );
}

pub async fn heartbeat(running: &mut RunningSession, id: &str) {
    running
        .peer
        .send_text(json!([2, id, "Heartbeat", {}]).to_string())
        .await
        .unwrap();
    respond_heartbeat(running).await;
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([3, id, {"currentTime":"2026-09-01T02:00:02Z"}])
    );
}

pub async fn respond_heartbeat(running: &mut RunningSession) {
    let incoming = timeout(TEST_BOUND, running.outputs.incoming.receive())
        .await
        .expect("station CALL handoff remains responsive")
        .expect("application station CALL");
    assert!(matches!(
        incoming.call.observation,
        ChargerObservation::Heartbeat {
            protocol: ProtocolEdition::Ocpp201
        }
    ));
    incoming
        .responder
        .respond(&json!({"currentTime":"2026-09-01T02:00:02Z"}))
        .expect("reserved station reply budget");
}

pub async fn shutdown(running: RunningSession, store: Store) {
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

pub async fn scope_denials(
    running: &mut RunningSession,
    commands: &Commands,
    snapshot: &StationSnapshot,
    index: usize,
    native_id: i32,
    payload: Value,
    resource: ResourceRef,
) {
    let widened = report_request(
        snapshot,
        &format!("scope-station-denied-{index}"),
        "GetReport",
        json!({"requestId":100 + native_id}),
        snapshot.station.clone(),
    );
    assert_eq!(
        commands.submit(widened).await.unwrap_err().code(),
        CommandAdmissionErrorCode::Unauthorized
    );
    let mut escaped = payload;
    escaped["requestId"] = json!(200 + native_id);
    escaped["componentVariable"][0]["component"]["evse"]["id"] = json!(2);
    let rejected = commands
        .submit(report_request(
            snapshot,
            &format!("scope-native-denied-{index}"),
            "GetReport",
            escaped,
            resource,
        ))
        .await
        .unwrap();
    assert!(matches!(
        rejected.lifecycle,
        CommandLifecycle::Rejected { ref error }
            if error.code == CommandErrorCode::InvalidParameters
    ));
    silence(running).await;
}

pub async fn complete_empty_report(
    running: &mut RunningSession,
    store: &Store,
    id: &str,
    fragment_id: &str,
    native_id: i32,
) {
    notify(
        running,
        notification(fragment_id, native_id, 0, false, vec![]),
    )
    .await;
    let evidence = terminal(store, id).await.device_model_201.unwrap();
    assert_eq!(evidence.native_ack, Some(DeviceReportAck201::Accepted));
    let DeviceReportState201::Complete {
        progress, items, ..
    } = evidence.report
    else {
        panic!("native empty final report must complete");
    };
    assert_eq!((progress.fragments, progress.items), (1, 0));
    assert!(items.is_empty());
}

pub async fn fresh_inventory(
    running: &mut RunningSession,
    commands: &Commands,
    snapshot: &StationSnapshot,
    store: &Store,
    index: usize,
    native_id: i32,
) {
    let fresh_id = format!("terminal-fresh-{index}");
    start_report(
        running,
        commands,
        report_request(
            snapshot,
            &fresh_id,
            "GetReport",
            json!({"requestId":native_id}),
            snapshot.station.clone(),
        ),
    )
    .await;
    notify(
        running,
        notification(
            &format!("terminal-fresh-final-{index}"),
            native_id,
            0,
            false,
            vec![item(json!({"name":"Ctrlr"}), "Fresh")],
        ),
    )
    .await;
    let evidence = terminal(store, &fresh_id).await.device_model_201.unwrap();
    let DeviceReportState201::Complete {
        progress, items, ..
    } = evidence.report
    else {
        panic!("explicit fresh inventory");
    };
    assert_eq!((progress.fragments, progress.items), (1, 1));
    assert_eq!(items[0].variable.name, "Fresh");
}
