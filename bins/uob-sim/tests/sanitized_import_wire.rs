#![allow(clippy::result_large_err)]
mod control_support;
use control_support::{Fixture, control_document, finished, start};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

// The namespace harness runs this actual wire test; ordinary runs fail closed outside staging.
#[tokio::test]
#[ignore = "requires the root-owned isolated staging network; scripts/test-staging-network.sh"]
async fn sanitized_replay_only_emits_reidentified_test_status_for_both_versions() {
    for (version, protocol, kind) in [
        ("1.6", "ocpp1.6", "sanitized_capture"),
        ("2.0.1", "ocpp2.0.1", "sanitized_snapshot"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let effects = Arc::new(Mutex::new(Vec::new()));
        let seen = effects.clone();
        let peer = peer(listener, seen, version, protocol);
        let fixture = Fixture::new(
            &json!({"schema_version":1,"kind":kind,"records":[
                {"station_slot":0,"connector_slot":0,"status":"Unavailable"}
            ]})
            .to_string(),
        );
        fixture.write(
            "simulator.toml",
            &format!(
                r#"schema_version = 1
[[stations]]
id = "staging-import-peer"
endpoint = "ws://{address}/staging-import-peer"
ocpp_version = "{version}"
"#
            ),
        );
        fixture.write(
            "control.toml",
            &(control_document("staging") + "sanitized_import = true\n"),
        );
        let server = fixture.server();
        let router = server.router();
        let mut identities = BTreeSet::new();
        for _ in 0..2 {
            let id = start(&router).await;
            let report = finished(&router, id).await;
            assert_eq!(report["environment"], "staging");
            assert_eq!(report["status"], "passed", "{report}");
            for event in report["events"].as_array().unwrap() {
                let identity = event["id"].as_str().unwrap();
                assert!(identity.starts_with("staging-import-"));
                assert!(identities.insert(identity.to_owned()), "reused evidence ID");
                if let Some(station) = event["station_id"].as_str() {
                    assert_eq!(station, "staging-import-peer");
                }
                assert!(event.get("detail").is_none());
            }
        }
        server.shutdown().await;
        tokio::time::timeout(Duration::from_secs(5), peer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(effects.lock().unwrap().len(), 2);
    }
}

fn peer(
    listener: TcpListener,
    seen: Arc<Mutex<Vec<Value>>>,
    version: &'static str,
    protocol: &'static str,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for _ in 0..2 {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(tcp, move |
                    request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                    mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(request.uri().path(), "/staging-import-peer");
                    assert!(request.headers().get("Authorization").is_none());
                    assert_eq!(request.headers()["Sec-WebSocket-Protocol"], protocol);
                    response.headers_mut().insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
                    Ok(response)
                }).await.unwrap();
            let mut calls = 0;
            while let Some(message) = socket.next().await {
                let Ok(Message::Text(text)) = message else {
                    break;
                };
                let call: Value = serde_json::from_str(&text).unwrap();
                let result = match call[2].as_str().unwrap() {
                    "BootNotification" => {
                        assert_eq!(calls, 0);
                        let expected = if version == "1.6" {
                            json!({"chargePointVendor":"UOB","chargePointModel":"Sanitized import"})
                        } else {
                            json!({"reason":"PowerUp","chargingStation":{"vendorName":"UOB","model":"Sanitized import"}})
                        };
                        assert_eq!(call[3], expected);
                        json!({"status":"Accepted","interval":60,"currentTime":"2000-01-01T00:00:00Z"})
                    }
                    "StatusNotification" => {
                        assert_eq!(calls, 1);
                        let expected = if version == "1.6" {
                            json!({"connectorId":1,"status":"Unavailable","errorCode":"NoError"})
                        } else {
                            json!({"evseId":1,"connectorId":1,"connectorStatus":"Unavailable","timestamp":"2000-01-01T00:00:00Z"})
                        };
                        assert_eq!(call[3], expected);
                        seen.lock().unwrap().push(call[3].clone());
                        json!({})
                    }
                    other => panic!("import dispatched unexpected operation {other}"),
                };
                calls += 1;
                socket
                    .send(Message::text(json!([3, call[1], result]).to_string()))
                    .await
                    .unwrap();
            }
            assert_eq!(calls, 2);
        }
    })
}
