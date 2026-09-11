use super::*;
use crate::endpoint_support::{self, SECRET, TEST_BOUND};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::{net::TcpListener, time::timeout};
use uob_contracts::Environment;
use uob_hostile_websocket_peer::{Peer, PeerConfig};
use uob_protocol_adapter::{
    CallSessionConfiguration, OcppEndpoint, StationAuthenticationMode, spawn_call_session,
    v201::complete_registration,
};

#[tokio::test]
async fn authenticated_wire_replies_follow_durable_decisions() {
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
    let authorization = STANDARD.encode([b"alpha:".as_slice(), SECRET].concat());
    let mut peer = Peer::connect(PeerConfig {
        endpoint: format!("ws://{address}/ocpp/alpha"),
        subprotocol: "ocpp2.0.1".to_owned(),
        max_outbound_bytes: 65536,
        max_inbound_bytes: 65536,
        observation_capacity: 8,
        authorization: Some(format!("Basic {authorization}")),
    })
    .await
    .unwrap();
    let connection = timeout(TEST_BOUND, connections.receive())
        .await
        .unwrap()
        .unwrap();
    let (_handle, mut outputs, task) = spawn_call_session(
        connection,
        &application,
        CallSessionConfiguration {
            pending_call_capacity: 4,
            incoming_call_capacity: 4,
            diagnostic_capacity: 8,
            response_timeout: TEST_BOUND,
        },
    )
    .unwrap();
    let db = Database::new();
    let store = db.open();
    let mut state = snapshot();
    // Rebind the synthetic snapshot to the authenticated test station, preserving connector shape.
    state.station.station_id = uob_contracts::StationId::new("alpha").unwrap();
    state.station.bridge_id = application.identity().bridge_id.clone();
    for resource in &mut state.resources {
        resource.resource.station_id = state.station.station_id.clone();
        resource.resource.bridge_id = state.station.bridge_id.clone();
    }
    for (frame, decision, second) in [
        (BOOT, RegistrationDecision::Pending, 0),
        (HEARTBEAT, RegistrationDecision::Pending, 1),
        (BOOT, RegistrationDecision::Accepted, 10),
        (STATUS, RegistrationDecision::Accepted, 11),
        (HEARTBEAT, RegistrationDecision::Accepted, 12),
    ] {
        // Unique IDs represent distinct retry calls; duplicate IDs are owned by call lifecycle.
        let mut value: Value = serde_json::from_slice(frame).unwrap();
        value[1] = json!(format!("wire-{second}"));
        peer.send_text(value.to_string()).await.unwrap();
        let incoming = timeout(TEST_BOUND, outputs.incoming.receive())
            .await
            .unwrap()
            .unwrap();
        assert!(
            timeout(Duration::from_millis(20), peer.receive())
                .await
                .is_err(),
            "no speculative response while application is delayed"
        );
        match complete_registration(incoming.call, &store, &mut state, decision, 10, now(second))
            .await
        {
            Ok(reply) => {
                assert_eq!(persisted(&store).await, state);
                incoming.responder.respond(&reply[2]).unwrap();
            }
            Err(error) => incoming.responder.reject(error).unwrap(),
        }
        let response = timeout(TEST_BOUND, peer.receive())
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response[1], value[1]);
        if second == 1 {
            assert_eq!(response[0], 4);
            assert_eq!(response[2], "ProtocolError");
        } else {
            assert_eq!(response[0], 3);
        }
    }
    assert_eq!(state.resources[0].availability, AvailabilityState::Faulted);
    task.shutdown(TEST_BOUND).await.unwrap();
    server.abort();
}
