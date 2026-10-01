use crate::support::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;

fn notification(id: &str, request: i32) -> serde_json::Value {
    json!([2,id,"NotifyReport",{"requestId":request,"generatedAt":"2026-09-01T02:00:01Z","seqNo":0,"reportData":[{"component":{"name":"SecurityCtrlr","customData":{"vendorId":"SECRET-VENDOR"}},"variable":{"name":"BasicAuthPassword"},"variableAttribute":[{"value":"SECRET-PASSWORD","mutability":"WriteOnly"},{"type":"Target"}],"variableCharacteristics":{"dataType":"string","supportsMonitoring":false,"valuesList":"SECRET-LIST"}}]}])
}

#[tokio::test]
async fn final_report_before_ack_is_private_pending_then_promoted_after_acceptance() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    let mut external = command(
        &snapshot,
        "report-before-ack",
        protocol(
            "GetBaseReport",
            json!({"requestId":-17,"reportBase":"FullInventory"}),
        ),
    );
    external.request.resource = snapshot.station.clone();
    let submitted = tokio::spawn(async move { commands.submit(external).await.unwrap() });
    receive_json(&mut running.peer).await;
    running
        .peer
        .send_text(notification("fragment-before-ack", -17).to_string())
        .await
        .unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([3, "fragment-before-ack", {}])
    );
    // Drain the same ordered worker after the collector has staged the complete payload.
    tokio::task::yield_now().await;
    let request = RequestId::new("report-before-ack").unwrap();
    let pending = store
        .command_result_by_request_id(request.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.device_model_201.unwrap().report,
        DeviceReportState201::Pending
    );
    running.peer.send_text(json!([3,"report-before-ack",{"status":"Accepted","statusInfo":{"reasonCode":"private","additionalInfo":"SECRET-STATUS"}}]).to_string()).await.unwrap();
    submitted.await.unwrap();
    let result = wait_terminal(&store, request).await;
    let bytes = serde_json::to_string(&result).unwrap();
    for secret in [
        "SECRET-PASSWORD",
        "SECRET-LIST",
        "SECRET-VENDOR",
        "SECRET-STATUS",
    ] {
        assert!(!bytes.contains(secret));
    }
    let DeviceReportState201::Complete {
        progress, items, ..
    } = result.device_model_201.unwrap().report
    else {
        panic!("complete report");
    };
    assert_eq!(progress.items, 1);
    assert_eq!(progress.fragments, 1);
    assert_eq!(items[0].attributes.len(), 2);
    assert_eq!(
        items[0].attributes[0].value,
        DeviceValue201 {
            present: true,
            redacted: true,
            empty: false,
            value: None
        }
    );
    assert_eq!(
        items[0].attributes[1].value,
        DeviceValue201 {
            present: false,
            redacted: false,
            empty: false,
            value: None
        }
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn report_without_native_acknowledgement_never_becomes_complete() {
    let mut running = session("ocpp2.0.1", Duration::from_millis(100)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    let mut external = command(
        &snapshot,
        "report-missing-ack",
        protocol("GetReport", json!({"requestId":i32::MAX})),
    );
    external.request.resource = snapshot.station.clone();
    let submitted = tokio::spawn(async move { commands.submit(external).await.unwrap() });
    receive_json(&mut running.peer).await;
    running
        .peer
        .send_text(notification("fragment-missing-ack", i32::MAX).to_string())
        .await
        .unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([3, "fragment-missing-ack", {}])
    );
    submitted.await.unwrap();
    let result = wait_terminal(&store, RequestId::new("report-missing-ack").unwrap()).await;
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    let DeviceReportState201::Incomplete { reason, progress } =
        result.device_model_201.unwrap().report
    else {
        panic!("incomplete report");
    };
    assert_eq!(reason, DeviceReportFailure201::MissingAcknowledgement);
    assert_eq!(progress.unwrap().items, 1);
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

async fn wait_terminal(store: &Store, request: RequestId) -> CommandResult {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let result = store
                .command_result_by_request_id(request.clone())
                .await
                .unwrap()
                .unwrap();
            if !result.device_model_201.as_ref().unwrap().report.pending() {
                return result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("durable collector completion")
}
