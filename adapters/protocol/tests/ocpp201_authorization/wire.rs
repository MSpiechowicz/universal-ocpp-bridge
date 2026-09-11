use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use endpoint_support::{SECRET, TEST_BOUND};
use tokio::{net::TcpListener, time::timeout};
use uob_contracts::Environment;
use uob_hostile_websocket_peer::{Peer, PeerConfig};
use uob_protocol_adapter::{
    CallSessionConfiguration, OcppEndpoint, StationAuthenticationMode, spawn_call_session,
};

#[tokio::test]
async fn authenticated_wire_authorization_reconnects_to_persisted_revocation() {
    let app = endpoint_support::application(Environment::Demo, None);
    let (endpoint, mut connections) = OcppEndpoint::new(
        endpoint_support::authenticator(StationAuthenticationMode::Credential, None),
        &app,
        4,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        endpoint.serve_plaintext(listener).await.unwrap();
    });
    let db = Database::new();
    let mut station = resource();
    station.bridge_id = app.identity().bridge_id.clone();
    station.station_id = uob_contracts::StationId::new("alpha").unwrap();
    let reference = reference(AUTHORIZE).await;
    let service = db.recover().await;
    allow(
        &service,
        station.clone(),
        reference.clone(),
        AuthorizationState::Active,
        1,
        Some(timestamp(5)),
    )
    .await;
    drop(service);
    for expected in ["Accepted", "Blocked"] {
        let service = db.recover().await;
        let mut peer = connect(address).await;
        let connection = timeout(TEST_BOUND, connections.receive())
            .await
            .unwrap()
            .unwrap();
        let (_handle, mut outputs, task) = spawn_call_session(
            connection,
            &app,
            CallSessionConfiguration {
                pending_call_capacity: 4,
                incoming_call_capacity: 4,
                diagnostic_capacity: 8,
                response_timeout: TEST_BOUND,
            },
        )
        .unwrap();
        peer.send_text(String::from_utf8(AUTHORIZE.to_vec()).unwrap())
            .await
            .unwrap();
        let incoming = timeout(TEST_BOUND, outputs.incoming.receive())
            .await
            .unwrap()
            .unwrap();
        assert!(
            timeout(Duration::from_millis(10), peer.receive())
                .await
                .is_err()
        );
        let reply = v201::complete_authorization(
            incoming.call,
            &station,
            &service,
            &LocalChargingIdentityProvider,
            &Clock(AtomicU8::new(1)),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        incoming.responder.respond(&reply[2]).unwrap();
        let response = timeout(TEST_BOUND, peer.receive())
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response[0], 3);
        assert_eq!(response[1], "fixture-201-authorize");
        assert_eq!(response[2]["idTokenInfo"]["status"], expected);
        allow(
            &service,
            station.clone(),
            reference.clone(),
            AuthorizationState::Revoked,
            2,
            None,
        )
        .await;
        task.shutdown(TEST_BOUND).await.unwrap();
    }
    server.abort();
}

async fn connect(address: std::net::SocketAddr) -> Peer {
    let authorization = STANDARD.encode([b"alpha:".as_slice(), SECRET].concat());
    Peer::connect(PeerConfig {
        endpoint: format!("ws://{address}/ocpp/alpha"),
        subprotocol: "ocpp2.0.1".into(),
        max_outbound_bytes: 65536,
        max_inbound_bytes: 65536,
        observation_capacity: 8,
        authorization: Some(format!("Basic {authorization}")),
    })
    .await
    .unwrap()
}
