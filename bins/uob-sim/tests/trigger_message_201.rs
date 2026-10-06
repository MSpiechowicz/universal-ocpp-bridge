use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use uob_sim::{
    OcppVersion, ProtocolClient, SimulatorAction, SimulatorCall, SimulatorClientConfig,
    SimulatorProtocolClient, TriggerObservation, TriggerReply, TriggerResponses,
};

const BOUND: Duration = Duration::from_secs(4);
type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

struct Ocpp201Handshake;
impl Callback for Ocpp201Handshake {
    fn on_request(self, _: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "ocpp2.0.1".parse().unwrap());
        Ok(response)
    }
}

async fn accept(listener: &TcpListener) -> Socket {
    let (stream, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_hdr_async(stream, Ocpp201Handshake)
        .await
        .unwrap()
}

async fn recv(socket: &mut Socket) -> Value {
    let frame = timeout(BOUND, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match frame {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("unexpected WebSocket frame: {other:?}"),
    }
}

async fn reply(socket: &mut Socket, id: &Value, payload: Value) {
    socket
        .send(Message::text(json!([3, id, payload]).to_string()))
        .await
        .unwrap();
}

fn response(action: &str) -> Value {
    match action {
        "BootNotification" => {
            json!({"currentTime":"2026-09-01T00:00:00Z", "interval":60, "status":"Accepted"})
        }
        "Heartbeat" => json!({"currentTime":"2026-09-01T00:00:00Z"}),
        "TransactionEvent" => json!({"idTokenInfo":{"status":"Accepted"}}),
        _ => json!({}),
    }
}

async fn trigger(
    socket: &mut Socket,
    id: &str,
    requested: &str,
    evse: Option<Value>,
    status: &str,
    count: usize,
) -> Vec<Value> {
    let mut payload = json!({"requestedMessage":requested});
    if let Some(evse) = evse {
        payload["evse"] = evse;
    }
    socket
        .send(Message::text(
            json!([2, id, "TriggerMessage", payload]).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(recv(socket).await, json!([3,id,{"status":status}]));
    let mut calls = Vec::new();
    for _ in 0..count {
        let call = recv(socket).await;
        assert_eq!(call[0], 2);
        assert_eq!(
            call[2],
            match requested {
                "SignChargingStationCertificate"
                | "SignV2GCertificate"
                | "SignCombinedCertificate" => "SignCertificate",
                other => other,
            }
        );
        reply(socket, &call[1], response(call[2].as_str().unwrap())).await;
        calls.push(call);
    }
    calls
}

fn config(endpoint: String) -> SimulatorClientConfig {
    SimulatorClientConfig {
        endpoint,
        credentials_file: None,
        version: OcppVersion::V2_0_1,
        request_timeout: Duration::from_secs(2),
        reconnect: false,
        command_capacity: 16,
        trace_capacity: 64,
        connectors: vec![],
        evse_connectors: vec![(1, 1), (1, 2), (2, 1)],
        trigger_responses: TriggerResponses::default(),
        trigger_observation: TriggerObservation::default(),
        local_authorization: None,
        local_authorization_file: None,
        reservation16: None,
        reservation201: None,
    }
}

async fn call(client: &SimulatorProtocolClient, action: SimulatorAction, payload: Value) -> Value {
    client
        .call(SimulatorCall { action, payload })
        .await
        .unwrap()
}

async fn assert_startup_and_idle_notifications(socket: &mut Socket) {
    let boot = trigger(
        socket,
        "boot",
        "BootNotification",
        Some(json!({"id":999})),
        "Accepted",
        1,
    )
    .await;
    assert_eq!(boot[0][3]["reason"], "Triggered");
    trigger(
        socket,
        "boot-again",
        "BootNotification",
        None,
        "Rejected",
        0,
    )
    .await;
    let heartbeat = trigger(
        socket,
        "heart",
        "Heartbeat",
        Some(json!({"id":999})),
        "Accepted",
        1,
    )
    .await;
    assert_eq!(heartbeat[0][3], json!({}));
    let log = trigger(socket, "log", "LogStatusNotification", None, "Accepted", 1).await;
    assert_eq!(log[0][3], json!({"status":"Idle"}));
    let firmware = trigger(
        socket,
        "firmware",
        "FirmwareStatusNotification",
        None,
        "Accepted",
        1,
    )
    .await;
    assert_eq!(firmware[0][3], json!({"status":"Idle"}));
    let publish = trigger(
        socket,
        "publish",
        "PublishFirmwareStatusNotification",
        None,
        "Accepted",
        1,
    )
    .await;
    assert_eq!(publish[0][3], json!({"status":"Idle"}));
}

#[tokio::test]
async fn eleven_native_classes_reply_first_and_follow_only_with_valid_current_information() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_startup_and_idle_notifications(&mut socket).await;
        for kind in [
            "SignChargingStationCertificate",
            "SignV2GCertificate",
            "SignCombinedCertificate",
        ] {
            trigger(&mut socket, kind, kind, None, "NotImplemented", 0).await;
        }
        trigger(
            &mut socket,
            "meter-no-reading",
            "MeterValues",
            Some(json!({"id":1})),
            "Rejected",
            0,
        )
        .await;
        trigger(
            &mut socket,
            "tx-no-current",
            "TransactionEvent",
            Some(json!({"id":2})),
            "Rejected",
            0,
        )
        .await;
        trigger(
            &mut socket,
            "status-missing-connector",
            "StatusNotification",
            Some(json!({"id":1})),
            "Rejected",
            0,
        )
        .await;
        let status = trigger(
            &mut socket,
            "status",
            "StatusNotification",
            Some(json!({"id":1,"connectorId":2})),
            "Accepted",
            1,
        )
        .await;
        assert_eq!(
            (
                status[0][3]["evseId"].as_u64(),
                status[0][3]["connectorId"].as_u64()
            ),
            (Some(1), Some(2))
        );
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_evse_targets_use_latest_meter_and_transaction_without_cross_evse_leakage() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let boot = recv(&mut socket).await;
        assert_eq!(boot[0], 2);
        assert_eq!(boot[2], "BootNotification");
        reply(&mut socket, &boot[1], response("BootNotification")).await;
        for _ in 0..2 {
            let call = recv(&mut socket).await;
            assert_eq!(call[2], "TransactionEvent");
            reply(&mut socket, &call[1], response("TransactionEvent")).await;
        }
        ready_tx.send(()).unwrap();
        let meter = trigger(
            &mut socket,
            "meter",
            "MeterValues",
            Some(json!({"id":2,"connectorId":1})),
            "Accepted",
            1,
        )
        .await;
        assert_eq!(meter[0][3]["evseId"], 2);
        assert!(meter[0][3].get("connectorId").is_none());
        assert_eq!(
            meter[0][3]["meterValue"][0]["sampledValue"][0]["context"],
            "Trigger"
        );
        let event = trigger(
            &mut socket,
            "event",
            "TransactionEvent",
            Some(json!({"id":2})),
            "Accepted",
            1,
        )
        .await;
        assert_eq!(event[0][3]["eventType"], "Updated");
        assert_eq!(event[0][3]["triggerReason"], "Trigger");
        assert_eq!(event[0][3]["transactionInfo"]["chargingState"], "Charging");
        assert_eq!(event[0][3]["seqNo"], 2);
        trigger(
            &mut socket,
            "unmeasured",
            "MeterValues",
            Some(json!({"id":1})),
            "Rejected",
            0,
        )
        .await;
        trigger(
            &mut socket,
            "missing",
            "StatusNotification",
            Some(json!({"id":1,"connectorId":99})),
            "Rejected",
            0,
        )
        .await;
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    let boot = call(
        &client,
        SimulatorAction::BootNotification,
        json!({"reason":"PowerUp","chargingStation":{"model":"Simulator","vendorName":"UOB"}}),
    )
    .await;
    assert_eq!(boot, response("BootNotification"));
    assert_eq!(client.accepted_registration(), Some(true));
    let start: Value = serde_json::from_str(include_str!("fixtures/ocpp201/start.json")).unwrap();
    let meter: Value = serde_json::from_str(include_str!("fixtures/ocpp201/meter.json")).unwrap();
    call(&client, SimulatorAction::StartTransaction, start[3].clone()).await;
    call(&client, SimulatorAction::MeterValues, meter[3].clone()).await;
    ready_rx.await.unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_denial_and_reconnect_drop_old_generation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut first = accept(&listener).await;
        trigger(
            &mut first,
            "denied",
            "LogStatusNotification",
            None,
            "Rejected",
            0,
        )
        .await;
        trigger(&mut first, "old", "Heartbeat", None, "Accepted", 0).await;
        first.close(None).await.unwrap();
        let mut next = timeout(BOUND, accept(&listener)).await.unwrap();
        let fresh = trigger(&mut next, "fresh", "Heartbeat", None, "Accepted", 1).await;
        assert_eq!(fresh[0][2], "Heartbeat");
        next.close(None).await.unwrap();
    });
    let mut options = config(endpoint);
    options.reconnect = true;
    options.trigger_responses.log_status_notification = TriggerReply::Rejected;
    options.trigger_observation.delay_ms = 1_500;
    let client = SimulatorProtocolClient::connect(options).await.unwrap();
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn omission_does_not_turn_accepted_native_reply_into_an_observed_call() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        trigger(&mut socket, "omitted", "Heartbeat", None, "Accepted", 0).await;
        trigger(
            &mut socket,
            "denied",
            "FirmwareStatusNotification",
            None,
            "Rejected",
            0,
        )
        .await;
        socket.close(None).await.unwrap();
    });
    let mut options = config(endpoint);
    options.trigger_observation.omit = true;
    options.trigger_responses.firmware_status_notification = TriggerReply::Rejected;
    let client = SimulatorProtocolClient::connect(options).await.unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_trigger_fields_do_not_steal_the_next_valid_callback_id() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        socket
            .send(Message::text(
                json!([2,"invalid","TriggerMessage",{
                    "requestedMessage":"Heartbeat","extra":true
                }])
                .to_string(),
            ))
            .await
            .unwrap();
        let error = recv(&mut socket).await;
        assert_eq!(error[0], 4);
        assert_eq!(error[1], "invalid");
        assert_eq!(error[2], "PropertyConstraintViolation");
        trigger(&mut socket, "valid", "Heartbeat", None, "Accepted", 1).await;
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}
