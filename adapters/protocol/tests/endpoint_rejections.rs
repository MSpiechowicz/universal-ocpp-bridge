mod endpoint_support;

use futures::SinkExt;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Message, http::StatusCode};
use uob_application::Application;
use uob_contracts::Environment;
use uob_protocol_adapter::{
    OcppEndpoint, StationAuthenticationMode, StationEndpointConfigurationError,
    StationEndpointServeError,
};

use endpoint_support::{SECRET, TEST_BOUND, authenticator, connect_plain, plaintext_endpoint};

#[tokio::test]
async fn unsupported_unknown_invalid_and_duplicate_handshakes_are_rejected() {
    let (address, mut accepted, _server) = plaintext_endpoint(None).await;

    assert_eq!(
        connect_plain(address, "alpha", SECRET, "ocpp9.9")
            .await
            .expect_err("unsupported protocol must fail"),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        endpoint_support::connect_plain_without_credential(address, "alpha", "ocpp1.6")
            .await
            .expect_err("missing credential must fail"),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        connect_plain(address, "unknown", SECRET, "ocpp1.6")
            .await
            .expect_err("unknown station must fail"),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        connect_plain(address, "alpha", b"incorrect-credential", "ocpp1.6")
            .await
            .expect_err("invalid credential must fail"),
        StatusCode::UNAUTHORIZED
    );

    let (first_client, _) = connect_plain(address, "alpha", SECRET, "ocpp1.6")
        .await
        .expect("first connection");
    let first = timeout(TEST_BOUND, accepted.receive())
        .await
        .expect("endpoint handoff exceeded bound")
        .expect("endpoint stopped");
    assert_eq!(
        connect_plain(address, "alpha", SECRET, "ocpp2.0.1")
            .await
            .expect_err("duplicate station must fail"),
        StatusCode::CONFLICT
    );
    drop(first_client);
    drop(first);

    let (_replacement, _) = connect_plain(address, "alpha", SECRET, "ocpp2.0.1")
        .await
        .expect("station can reconnect after prior ownership is released");
}

#[tokio::test]
async fn the_seventeenth_concurrent_station_is_rejected() {
    let application = endpoint_support::application(Environment::Demo, None);
    let (endpoint, mut accepted) =
        OcppEndpoint::new(endpoint_support::authenticator_many(17), &application, 17)
            .expect("endpoint");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        endpoint
            .serve_plaintext(listener)
            .await
            .expect("plaintext endpoint");
    });

    let mut clients = Vec::new();
    let mut connections = Vec::new();
    for index in 0..16 {
        let station = endpoint_support::station_name(index);
        let secret = endpoint_support::station_secret(index);
        let (client, _) = connect_plain(address, &station, secret.as_bytes(), "ocpp1.6")
            .await
            .expect("within station limit");
        clients.push(client);
        connections.push(
            timeout(TEST_BOUND, accepted.receive())
                .await
                .expect("endpoint handoff exceeded bound")
                .expect("endpoint stopped"),
        );
    }
    let station = endpoint_support::station_name(16);
    let secret = endpoint_support::station_secret(16);
    assert_eq!(
        connect_plain(address, &station, secret.as_bytes(), "ocpp1.6")
            .await
            .expect_err("station limit must fail"),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        application
            .health()
            .resources()
            .snapshot()
            .connected_stations,
        16
    );

    server.abort();
    drop(clients);
    drop(connections);
}

#[tokio::test]
async fn oversized_messages_are_closed_before_reaching_endpoint_consumers() {
    let (address, mut accepted, _server) = plaintext_endpoint(None).await;
    let (mut client, _) = connect_plain(address, "alpha", SECRET, "ocpp1.6")
        .await
        .expect("connection");
    let mut connection = timeout(TEST_BOUND, accepted.receive())
        .await
        .expect("endpoint handoff exceeded bound")
        .expect("endpoint stopped");

    client
        .send(Message::Binary(vec![0_u8; 256 * 1024 + 1].into()))
        .await
        .expect("client writes oversized message");
    let received = timeout(TEST_BOUND, connection.receive())
        .await
        .expect("oversized rejection exceeded bound")
        .expect("socket reports a terminal read result");
    assert!(received.is_err());
}

#[tokio::test]
async fn plaintext_and_queue_configuration_fail_closed() {
    let app = Application::new(
        endpoint_support::application(Environment::Demo, None)
            .identity()
            .clone(),
    );
    assert!(matches!(
        OcppEndpoint::new(
            authenticator(StationAuthenticationMode::Credential, None),
            &app,
            0,
        ),
        Err(StationEndpointConfigurationError::ConnectionCapacity)
    ));

    let production = endpoint_support::application(Environment::Production, None);
    let (endpoint, _accepted) = OcppEndpoint::new(
        authenticator(StationAuthenticationMode::Credential, None),
        &production,
        1,
    )
    .expect("endpoint");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    assert!(matches!(
        endpoint.serve_plaintext(listener).await,
        Err(StationEndpointServeError::PlaintextEnvironment)
    ));
}
