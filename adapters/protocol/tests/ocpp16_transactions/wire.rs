use super::support::*;
use crate::endpoint_support::{self, SECRET, TEST_BOUND};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{net::TcpListener, time::timeout};
use uob_application::{CommandClock, registration::RegistrationDecision};
use uob_contracts::Environment;
use uob_hostile_websocket_peer::{Peer, PeerConfig};
use uob_protocol_adapter::{
    CallSessionConfiguration, OcppEndpoint, StationAuthenticationMode, spawn_call_session, v16,
};

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the wire exchange and durable evidence in one scenario.
async fn authenticated_wire_waits_for_commit_and_reconnect_preserves_transaction() {
    let application = endpoint_support::application(Environment::Demo, None);
    let (endpoint, mut connections) = OcppEndpoint::new(
        endpoint_support::authenticator(StationAuthenticationMode::Credential, None),
        &application,
        4,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        endpoint.serve_plaintext(listener).await.unwrap();
    });
    let db = Database::new();
    let store = db.open();
    let (mut state, auth) = setup(&store).await;
    state.station.station_id = uob_contracts::StationId::new("alpha").unwrap();
    state.station.bridge_id = application.identity().bridge_id.clone();
    for r in &mut state.resources {
        r.resource.station_id = state.station.station_id.clone();
        r.resource.bridge_id = state.station.bridge_id.clone();
    }
    allow(
        &auth,
        &state,
        uob_application::AuthorizationState::Active,
        2,
        None,
    )
    .await;
    for frames in [vec![START], vec![START, STOP]] {
        let mut peer = Peer::connect(PeerConfig {
            endpoint: format!("ws://{address}/ocpp/alpha"),
            subprotocol: "ocpp1.6".to_owned(),
            authorization: Some(format!(
                "Basic {}",
                STANDARD.encode([b"alpha:".as_slice(), SECRET].concat())
            )),
            max_outbound_bytes: 65536,
            max_inbound_bytes: 65536,
            observation_capacity: 8,
        })
        .await
        .unwrap();
        let admitted = timeout(TEST_BOUND, connections.receive())
            .await
            .unwrap()
            .unwrap();
        let (handle, mut outputs, task) = spawn_call_session(
            admitted,
            &application,
            CallSessionConfiguration {
                pending_call_capacity: 4,
                incoming_call_capacity: 4,
                diagnostic_capacity: 8,
                response_timeout: TEST_BOUND,
            },
        )
        .unwrap();
        for frame in frames {
            peer.send_text(String::from_utf8(frame.to_vec()).unwrap())
                .await
                .unwrap();
            let incoming = timeout(TEST_BOUND, outputs.incoming.receive())
                .await
                .unwrap()
                .unwrap();
            assert!(
                timeout(Duration::from_millis(20), peer.receive())
                    .await
                    .is_err()
            );
            let sequence = if incoming.call.action.as_str() == "StopTransaction" {
                2
            } else {
                1
            };
            let ctx = context(&state, sequence);
            let response =
                v16::complete_transaction(incoming.call, &mut state, &services(&store, &auth), ctx)
                    .await
                    .unwrap();
            assert_eq!(persisted_wire(&store).await, state);
            incoming.responder.respond(&response[2]).unwrap();
            let response: Value = serde_json::from_str(
                &timeout(TEST_BOUND, peer.receive())
                    .await
                    .unwrap()
                    .unwrap()
                    .into_text()
                    .unwrap(),
            )
            .unwrap();
            if sequence == 1 {
                assert_eq!(
                    response[2],
                    json!({"transactionId":1,"idTagInfo":{"status":"Accepted"}})
                );
            } else {
                assert_eq!(response[2], json!({}));
            }
        }
        drop(peer);
        drop(handle);
        task.shutdown(TEST_BOUND).await.unwrap();
        state = persisted_wire(&store).await;
        // Re-registration shares the transaction snapshot, rather than creating another owner.
        v16::registration_call(
            include_bytes!(
                "../../../../tests/ocpp-fixtures/corpus/wire/1.6/boot-notification.json"
            ),
            &store,
            &mut state,
            RegistrationDecision::Accepted,
            60,
            Clock.now(),
        )
        .await
        .unwrap();
    }
    server.abort();
}

async fn persisted_wire(store: &Store) -> uob_contracts::StationSnapshot {
    use uob_application::OperationalStore;
    store
        .read_snapshots(uob_application::SnapshotQuery {
            after: None,
            limit: uob_application::PageLimit::new(100).unwrap(),
        })
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|s| s.station.station_id.as_str() == "alpha")
        .unwrap()
}
