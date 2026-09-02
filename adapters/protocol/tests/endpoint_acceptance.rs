mod endpoint_support;

use tokio::time::timeout;
use uob_contracts::ProtocolEdition;

use endpoint_support::{
    TEST_BOUND, TestPki, connect_plain, connect_tls, plaintext_endpoint, tls_endpoint,
};

#[tokio::test]
async fn both_protocols_are_authenticated_over_real_wss_with_mtls() {
    let pki = TestPki::current("endpoint");
    let (address, mut accepted, _server) = tls_endpoint(&pki).await;

    for (protocol, expected) in [
        ("ocpp1.6", ProtocolEdition::Ocpp16j),
        ("ocpp2.0.1", ProtocolEdition::Ocpp201),
    ] {
        let (client, response) = connect_tls(address, &pki, protocol)
            .await
            .expect("authenticated WSS connection");
        assert_eq!(response.headers()["sec-websocket-protocol"], protocol);
        let connection = timeout(TEST_BOUND, accepted.receive())
            .await
            .expect("endpoint handoff exceeded bound")
            .expect("endpoint stopped");
        assert_eq!(connection.station().station_id.as_str(), "alpha");
        assert_eq!(connection.station().protocol, expected);
        drop(client);
        drop(connection);
    }
}

#[tokio::test]
async fn target_outages_do_not_block_safe_local_station_admission() {
    for target in ["mqtt", "ems-http", "ems-mqtt"] {
        for protocol in ["ocpp1.6", "ocpp2.0.1"] {
            let (address, mut accepted, _server) = plaintext_endpoint(Some(target)).await;
            let (client, _) = connect_plain(address, "alpha", endpoint_support::SECRET, protocol)
                .await
                .expect("target-independent connection");
            let connection = timeout(TEST_BOUND, accepted.receive())
                .await
                .expect("endpoint handoff exceeded bound")
                .expect("endpoint stopped");
            assert_eq!(connection.station().station_id.as_str(), "alpha");
            drop(client);
            drop(connection);
        }
    }
}
