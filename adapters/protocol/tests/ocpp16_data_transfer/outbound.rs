use super::support::*;
use crate::endpoint_support::{self, SECRET, TEST_BOUND};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};
use uob_application::data_transfer::{Observation, OpaqueData, Registry, Status};
use uob_contracts::{CorrelationId, Environment, StationId, StationSnapshot, TypedValue};
use uob_protocol_adapter::{
    CallSessionConfiguration, CallSessionHandle, CallSessionTask, spawn_call_session,
    v16::data_transfer::{OutboundOutcome, OutboundRequest, send_data_transfer},
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
        let (peer, _) = endpoint_support::connect_plain(address, "alpha", SECRET, "ocpp1.6")
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
        data: Some(OpaqueData::new("ping".to_owned()).unwrap()),
    }
}
fn context(request: &Observation) -> OutboundRequest<'_> {
    OutboundRequest {
        request,
        message_id: "fixture-16-outbound",
        correlation_id: CorrelationId::new("transfer-outbound").unwrap(),
    }
}
async fn station(store: &Store) -> StationSnapshot {
    let mut state = registered(store).await;
    state.station.station_id = StationId::new("alpha").unwrap();
    // Keep one durable station, rather than retaining the synthetic fixture's old identity.
    state
}
async fn outbound_status(store: &Store) -> String {
    use uob_application::{OperationalStore, PageLimit, SnapshotQuery};
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
        .find(|s| s.station.station_id.as_str() == "alpha")
        .unwrap();
    match value(state, "outbound_status").unwrap() {
        TypedValue::Text(s) => s.clone(),
        other => panic!("unexpected outcome {other:?}"),
    }
}
async fn respond(peer: &mut Peer, store: &Store, reply: Value) {
    let wire = timeout(TEST_BOUND, peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    let expected: Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/ocpp-fixtures/corpus/wire/1.6/data-transfer-outbound.json"
    ))
    .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&wire).unwrap(), expected);
    assert_eq!(outbound_status(store).await, "TransmissionUncertain");
    peer.send(Message::Text(
        json!([3, "fixture-16-outbound", reply]).to_string().into(),
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn outbound_preserves_native_results_and_bounds_without_persisting_data() {
    let replies = [
        (
            json!({"status":"Accepted","data":"secret://fixture/response"}),
            Some(Status::Accepted),
        ),
        (json!({"status":"Rejected"}), Some(Status::Rejected)),
        (
            json!({"status":"UnknownVendorId"}),
            Some(Status::UnknownVendorId),
        ),
        (
            json!({"status":"UnknownMessageId","data":"explanation"}),
            Some(Status::UnknownMessageId),
        ),
        (json!({"status":"UnknownVendorId","data":"forbidden"}), None),
        (json!({"status":"Accepted","data":"x".repeat(16385)}), None),
        (json!({"status":"Accepted","extra":true}), None),
    ];
    for (reply, expected) in replies {
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
            respond(&mut wire.peer, &store, reply.clone())
        );
        match (result.unwrap(), expected) {
            (OutboundOutcome::Reply(response), Some(status)) => {
                assert_eq!(response.status, status);
                assert_eq!(
                    response.data.as_ref().map(OpaqueData::expose),
                    reply.get("data").and_then(Value::as_str)
                );
                assert!(!format!("{response:?}").contains("secret://fixture"));
                assert_eq!(outbound_status(&store).await, status.as_str());
            }
            (OutboundOutcome::TransmissionUncertain, None) => {
                assert_eq!(outbound_status(&store).await, "TransmissionUncertain");
            }
            (actual, expected) => panic!("unexpected {actual:?}, wanted {expected:?}"),
        }
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
async fn outbound_timeout_disconnect_and_restart_never_replay() {
    for disconnect in [false, true] {
        let mut wire = Wire::new().await;
        let db = Database::new();
        let store = db.open();
        let mut state = station(&store).await;
        let registry = registry(Arc::new(Probe));
        let request = observation();
        let peer = async {
            let frame = timeout(TEST_BOUND, wire.peer.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(frame.is_text());
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
    let empty = Registry::new(vec![]).unwrap();
    let request = observation();
    assert!(
        send_data_transfer(
            &wire.handle,
            &store,
            &mut state,
            &empty,
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
