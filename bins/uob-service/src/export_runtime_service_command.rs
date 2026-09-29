use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

use super::{ALPHA, Fixture, PRIVILEGED_TOKEN, ocpp_call};

// Same supported TriggerMessage command contract as tests/remote_trigger.rs.
pub(super) async fn trigger_while_exporter_hung(fixture: &Fixture) {
    let mut request = format!("ws://127.0.0.1:{}/ocpp/station-a", fixture.charging)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp1.6".parse().unwrap());
    request
        .headers_mut()
        .insert("Authorization", format!("Basic {ALPHA}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let boot = ocpp_call(
        &mut socket,
        json!([
            2, "outage-command-boot", "BootNotification",
            {"chargePointVendor":"TestVendor", "chargePointModel":"TestModel"}
        ]),
    )
    .await;
    assert_eq!(boot[2]["status"], "Accepted");

    let command_id = format!("outage-trigger-{}", uuid::Uuid::new_v4());
    let command = json!({
        "request_id": command_id,
        "resource": {"bridge_id":"bridge-1", "station_id":"station-a"},
        "operation": {"kind":"ocpp", "parameters": {
            "protocol":"ocpp16j", "action":"TriggerMessage",
            "payload_schema":"urn:OCPP:1.6:2019:12:TriggerMessageRequest",
            "payload":{"requestedMessage":"Heartbeat"}
        }},
        "expires_at":"2099-01-01T00:00:00Z"
    });
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/api/v1/commands", fixture.management);
    let submission = tokio::spawn(async move {
        client
            .post(url)
            .bearer_auth(PRIVILEGED_TOKEN)
            .json(&command)
            .send()
            .await
            .unwrap()
    });
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .expect("command reached station without waiting on exporter")
        .expect("station remained connected")
        .unwrap();
    let Message::Text(text) = frame else {
        panic!("expected OCPP command frame")
    };
    let call: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(call[0], 2);
    assert_eq!(call[2], "TriggerMessage");
    assert_eq!(call[3]["requestedMessage"], "Heartbeat");
    socket
        .send(Message::Text(
            json!([3, call[1], {"status":"Accepted"}])
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), submission)
        .await
        .expect("management command completed without waiting on exporter")
        .unwrap();
    assert_eq!(
        response.status(),
        202,
        "{}",
        response.text().await.unwrap_or_default()
    );
    let accepted: Value = response.json().await.unwrap();
    assert_eq!(
        accepted["result"]["trigger_observation"]["native_response"],
        "Accepted"
    );
    assert_eq!(accepted["request_id"], command_id);
}
