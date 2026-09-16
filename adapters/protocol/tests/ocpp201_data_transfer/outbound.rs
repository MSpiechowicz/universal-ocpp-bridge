use super::support::*;
use crate::endpoint_support::{self, SECRET, TEST_BOUND};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};
use uob_application::{
    OperationalStore, PageLimit, SnapshotQuery,
    data_transfer201::{Observation, OpaqueData, Registry},
};
use uob_contracts::{CorrelationId, Environment, StationSnapshot, TypedValue};
use uob_protocol_adapter::{
    CallSessionConfiguration, CallSessionHandle, CallSessionTask, RemoteCallError,
    spawn_call_session,
    v201::data_transfer::{OutboundOutcome, OutboundRequest, send_data_transfer},
};

type Peer = WebSocketStream<MaybeTlsStream<TcpStream>>;
struct Wire {
    peer: Peer,
    handle: CallSessionHandle,
    task: CallSessionTask,
    _server: endpoint_support::ServerTask,
}
impl Wire {
    async fn new() -> Self {
        let (address, mut connections, server) = endpoint_support::plaintext_endpoint(None).await;
        let (peer, _) = endpoint_support::connect_plain(address, "alpha", SECRET, "ocpp2.0.1")
            .await
            .unwrap();
        let connection = timeout(TEST_BOUND, connections.receive())
            .await
            .unwrap()
            .unwrap();
        let application = endpoint_support::application(Environment::Demo, None);
        let (handle, _, task) = spawn_call_session(
            connection,
            &application,
            CallSessionConfiguration {
                pending_call_capacity: 4,
                incoming_call_capacity: 4,
                diagnostic_capacity: 8,
                response_timeout: Duration::from_millis(150),
            },
        )
        .unwrap();
        Self {
            peer,
            handle,
            task,
            _server: server,
        }
    }
}
fn observation() -> Observation {
    Observation {
        vendor_id: VENDOR.to_owned(),
        message_id: Some("Probe".to_owned()),
        data: Some(OpaqueData::new(json!(["request", 1, false, null])).unwrap()),
        custom_data: Some(
            OpaqueData::new(json!({"vendorId":FIXTURE_VENDOR,"opaque":{"scope":"outbound"}}))
                .unwrap(),
        ),
    }
}
fn context(request: &Observation) -> OutboundRequest<'_> {
    OutboundRequest {
        request,
        message_id: "fixture-201-outbound",
        correlation_id: CorrelationId::new("transfer-201-outbound").unwrap(),
    }
}
async fn station(store: &Store) -> StationSnapshot {
    registered(store, Some("alpha")).await
}
async fn outbound_status(store: &Store) -> String {
    let states = store
        .read_snapshots(SnapshotQuery {
            after: None,
            limit: PageLimit::new(10).unwrap(),
        })
        .await
        .unwrap()
        .items;
    let state = states
        .iter()
        .find(|state| state.station.station_id.as_str() == "alpha")
        .unwrap();
    match value(state, "outbound_status").unwrap() {
        TypedValue::Text(outcome) => outcome.clone(),
        other => panic!("unexpected outbound outcome {other:?}"),
    }
}
async fn receive_request(peer: &mut Peer, store: &Store) {
    let wire = timeout(TEST_BOUND, peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    let expected: Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/data-transfer-outbound.json"
    ))
    .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&wire).unwrap(), expected);
    assert_eq!(outbound_status(store).await, "TransmissionUncertain");
}
async fn respond(peer: &mut Peer, store: &Store, frame: Value) {
    receive_request(peer, store).await;
    peer.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn outbound_preserves_native_results_and_bounds_without_persisting_opaque_content() {
    let replies = [
        json!({"status":"Accepted","data":null,"customData":{"vendorId":FIXTURE_VENDOR,"secret":"secret://fixture/201-response"},"statusInfo":{"reasonCode":"secret://reason","additionalInfo":"secret://fixture/201-info","customData":{"vendorId":FIXTURE_VENDOR,"opaque":[true,null]}}}),
        json!({"status":"Rejected","data":false,"statusInfo":{"reasonCode":"PolicyDenied"}}),
        json!({"status":"UnknownVendorId","data":[1,true,null]}),
        json!({"status":"UnknownMessageId","data":42,"customData":{"vendorId":FIXTURE_VENDOR}}),
    ];
    for reply in replies {
        let mut wire = Wire::new().await;
        let db = Database::new();
        let store = db.open();
        let mut state = station(&store).await;
        let registry = registry(Arc::new(Probe));
        let request = observation();
        let expected_status = reply["status"].as_str().unwrap().to_owned();
        let (result, ()) = tokio::join!(
            send_data_transfer(
                &wire.handle,
                &store,
                &mut state,
                &registry,
                context(&request),
                now(1)
            ),
            respond(
                &mut wire.peer,
                &store,
                json!([3, "fixture-201-outbound", reply.clone()])
            )
        );
        let OutboundOutcome::Reply(response) = result.unwrap() else {
            panic!("expected native reply")
        };
        assert_eq!(response.status.as_str(), expected_status);
        assert_eq!(
            response.data.as_ref().map(OpaqueData::expose),
            reply.get("data")
        );
        assert_eq!(
            response.custom_data.as_ref().map(OpaqueData::expose),
            reply.get("customData")
        );
        assert_eq!(
            response
                .status_info
                .as_ref()
                .and_then(|info| info.additional_info.as_deref()),
            reply
                .get("statusInfo")
                .and_then(|info| info.get("additionalInfo"))
                .and_then(Value::as_str)
        );
        let debug = format!("{response:?}");
        for secret in ["secret://fixture", "secret://reason"] {
            assert!(!debug.contains(secret));
        }
        assert_eq!(outbound_status(&store).await, expected_status);
        assert!(
            !serde_json::to_string(&state)
                .unwrap()
                .contains("secret://fixture")
        );
        wire.task.shutdown(TEST_BOUND).await.unwrap();
        shutdown(&store).await;
    }
}

#[tokio::test]
async fn outbound_rejections_errors_and_malformed_replies_are_durable() {
    for (frame, expected) in [
        (
            json!([4, "fixture-201-outbound", "SecurityError", "denied", {}]),
            "CallError",
        ),
        (
            json!([3,"fixture-201-outbound",{"status":"Invented"}]),
            "TransmissionUncertain",
        ),
        (
            json!([3,"fixture-201-outbound",{"status":"Accepted","statusInfo":{"reasonCode":"x".repeat(21)}}]),
            "TransmissionUncertain",
        ),
        (
            json!([3,"fixture-201-outbound",{"status":"Accepted","customData":{}}]),
            "TransmissionUncertain",
        ),
    ] {
        let mut wire = Wire::new().await;
        let db = Database::new();
        let store = db.open();
        let mut state = station(&store).await;
        let registry = registry(Arc::new(Probe));
        let request = observation();
        let (result, ()) = tokio::join!(
            send_data_transfer(
                &wire.handle,
                &store,
                &mut state,
                &registry,
                context(&request),
                now(1)
            ),
            respond(&mut wire.peer, &store, frame)
        );
        match result.unwrap() {
            OutboundOutcome::Error(RemoteCallError { .. }) if expected == "CallError" => {}
            OutboundOutcome::TransmissionUncertain if expected == "TransmissionUncertain" => {}
            outcome => panic!("unexpected {outcome:?}"),
        }
        assert_eq!(outbound_status(&store).await, expected);
        wire.task.shutdown(TEST_BOUND).await.unwrap();
        shutdown(&store).await;
    }
}

#[tokio::test]
async fn outbound_timeout_disconnect_and_restart_never_replay() {
    for disconnect in [false, true] {
        let mut wire = Wire::new().await;
        let db = Database::new();
        let store = db.open();
        let mut state = station(&store).await;
        let registry = registry(Arc::new(Probe));
        let request = observation();
        let peer = async {
            receive_request(&mut wire.peer, &store).await;
            if disconnect {
                wire.peer.close(None).await.unwrap();
            }
        };
        let (result, ()) = tokio::join!(
            send_data_transfer(
                &wire.handle,
                &store,
                &mut state,
                &registry,
                context(&request),
                now(2)
            ),
            peer
        );
        let expected = if disconnect {
            "TransmissionUncertain"
        } else {
            "TimedOut"
        };
        assert!(matches!(
            (result.unwrap(), disconnect),
            (OutboundOutcome::TransmissionUncertain, true) | (OutboundOutcome::TimedOut, false)
        ));
        assert_eq!(outbound_status(&store).await, expected);
        wire.task.shutdown(TEST_BOUND).await.unwrap();
        shutdown(&store).await;
        drop(store);
        let store = db.open();
        assert_eq!(outbound_status(&store).await, expected);
        let mut reconnect = Wire::new().await;
        assert!(
            timeout(Duration::from_millis(30), reconnect.peer.next())
                .await
                .is_err()
        );
        reconnect.task.shutdown(TEST_BOUND).await.unwrap();
        shutdown(&store).await;
    }
}

#[tokio::test]
async fn outbound_denied_capability_and_failed_intent_commit_send_nothing() {
    let mut wire = Wire::new().await;
    let db = Database::new();
    let store = db.open();
    let mut state = station(&store).await;
    let before = state.clone();
    let request = observation();
    assert!(
        send_data_transfer(
            &wire.handle,
            &store,
            &mut state,
            &Registry::new(vec![]).unwrap(),
            context(&request),
            now(1)
        )
        .await
        .is_err()
    );
    assert_eq!(state, before);
    let registry = registry(Arc::new(Probe));
    shutdown(&store).await;
    assert!(
        send_data_transfer(
            &wire.handle,
            &store,
            &mut state,
            &registry,
            context(&request),
            now(1)
        )
        .await
        .is_err()
    );
    assert_eq!(state, before);
    assert!(
        timeout(Duration::from_millis(30), wire.peer.next())
            .await
            .is_err()
    );
    wire.task.shutdown(TEST_BOUND).await.unwrap();
}
