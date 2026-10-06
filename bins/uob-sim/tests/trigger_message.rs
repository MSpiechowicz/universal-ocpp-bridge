use futures::SinkExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use uob_sim::{
    ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient, TriggerReply,
};
#[path = "trigger_message/support.rs"]
mod support;
use support::{BOUND, Socket, accept, config, recv};

async fn assert_invalid_field_response(socket: &mut Socket, id: &str) {
    socket
        .send(Message::text(
            json!([2, id, "TriggerMessage", {
                "requestedMessage": "Heartbeat",
                "extra": true
            }])
            .to_string(),
        ))
        .await
        .unwrap();
    let error = recv(socket).await;
    assert_eq!(error[0], 4);
    assert_eq!(error[1], id);
    assert_eq!(error[2], "PropertyConstraintViolation");
}

async fn trigger(
    socket: &mut Socket,
    id: &str,
    message: &str,
    connector: Option<i64>,
    count: usize,
    status: &str,
) -> Vec<Value> {
    let mut request = json!({"requestedMessage":message});
    if let Some(connector) = connector {
        request["connectorId"] = json!(connector);
    }
    socket
        .send(Message::text(
            json!([2, id, "TriggerMessage", request]).to_string(),
        ))
        .await
        .unwrap();
    let confirmation = recv(socket).await;
    assert_eq!(confirmation, json!([3,id,{"status":status}]));
    let mut calls = Vec::new();
    for _ in 0..count {
        let call = recv(socket).await;
        assert_eq!(call[0], 2);
        assert_eq!(call[2], message);
        let reply = match message {
            "BootNotification" => {
                json!({"currentTime":"2026-09-01T00:00:00Z","interval":60,"status":"Accepted"})
            }
            "Heartbeat" => json!({"currentTime":"2026-09-01T00:00:00Z"}),
            _ => json!({}),
        };
        socket
            .send(Message::text(json!([3, call[1], reply]).to_string()))
            .await
            .unwrap();
        calls.push(call);
    }
    calls
}

async fn assert_unscoped_triggers(socket: &mut Socket) {
    let boot = trigger(socket, "boot", "BootNotification", Some(99), 1, "Accepted").await;
    assert_eq!(boot[0][3]["chargePointModel"], "Simulator");
    trigger(
        socket,
        "boot-again",
        "BootNotification",
        None,
        0,
        "Rejected",
    )
    .await;

    let heartbeat = trigger(socket, "heart", "Heartbeat", Some(0), 1, "Accepted").await;
    assert_eq!(heartbeat[0][3], json!({}));
    let diagnostics = trigger(
        socket,
        "diag",
        "DiagnosticsStatusNotification",
        Some(99),
        1,
        "Accepted",
    )
    .await;
    assert_eq!(diagnostics[0][3], json!({"status":"Idle"}));
    let firmware = trigger(
        socket,
        "firm",
        "FirmwareStatusNotification",
        None,
        1,
        "Accepted",
    )
    .await;
    assert_eq!(firmware[0][3], json!({"status":"Idle"}));
}

async fn assert_status_connector_scopes(socket: &mut Socket) {
    let station = trigger(
        socket,
        "station",
        "StatusNotification",
        Some(0),
        1,
        "Accepted",
    )
    .await;
    assert_eq!(station[0][3]["connectorId"], 0);
    let exact = trigger(
        socket,
        "exact",
        "StatusNotification",
        Some(2),
        1,
        "Accepted",
    )
    .await;
    assert_eq!(exact[0][3]["connectorId"], 2);
    let all = trigger(socket, "all", "StatusNotification", None, 3, "Accepted").await;
    assert_eq!(
        all.iter()
            .map(|call| call[3]["connectorId"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
}

async fn assert_rejected_triggers(socket: &mut Socket) {
    trigger(
        socket,
        "meters-without-reading",
        "MeterValues",
        None,
        0,
        "Rejected",
    )
    .await;
    trigger(
        socket,
        "invalid-meter",
        "MeterValues",
        Some(0),
        0,
        "Rejected",
    )
    .await;
    trigger(
        socket,
        "invalid-status",
        "StatusNotification",
        Some(42),
        0,
        "Rejected",
    )
    .await;
    // No hidden follow-up from denied requests: a fresh heartbeat must be the next frame.
    trigger(socket, "last", "Heartbeat", None, 1, "Accepted").await;
}

#[tokio::test]
async fn unknown_trigger_fields_return_callerror_without_sending_a_station_call() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_invalid_field_response(&mut socket, "invalid").await;
        trigger(&mut socket, "valid", "Heartbeat", None, 1, "Accepted").await;
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_trigger_envelopes_do_not_misassign_the_next_valid_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        socket
            .send(Message::text(
                json!([2, "malformed", "TriggerMessage", {
                    "requestedMessage": "Heartbeat",
                    "extra": true
                }, 0])
                .to_string(),
            ))
            .await
            .unwrap();
        trigger(&mut socket, "valid", "Heartbeat", None, 1, "Accepted").await;
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn trigger_arriving_during_handler_registration_keeps_the_next_id_aligned() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_invalid_field_response(&mut socket, "early").await;
        trigger(&mut socket, "later", "Heartbeat", None, 1, "Accepted").await;
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn emits_all_six_classes_after_matching_confirmation_and_respects_connector_scope() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_unscoped_triggers(&mut socket).await;
        assert_status_connector_scopes(&mut socket).await;
        assert_rejected_triggers(&mut socket).await;
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_denial_and_missing_observation_keep_native_reply_without_station_call() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        trigger(&mut socket, "deny", "Heartbeat", None, 0, "Rejected").await;
        trigger(
            &mut socket,
            "unsupported",
            "FirmwareStatusNotification",
            None,
            0,
            "NotImplemented",
        )
        .await;
        trigger(
            &mut socket,
            "missing",
            "DiagnosticsStatusNotification",
            None,
            0,
            "Accepted",
        )
        .await;
        trigger(&mut socket, "last", "BootNotification", None, 0, "Accepted").await;
        socket.close(None).await.unwrap();
    });
    let mut config = config(endpoint);
    config.trigger_responses.heartbeat = TriggerReply::Rejected;
    config.trigger_responses.firmware_status_notification = TriggerReply::NotImplemented;
    config.trigger_observation.omit = true;
    let client = SimulatorProtocolClient::connect(config).await.unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn reconnect_discards_delayed_trigger_from_previous_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut first = accept(&listener).await;
        trigger(&mut first, "old", "Heartbeat", None, 0, "Accepted").await;
        first.close(None).await.unwrap();

        let mut second = timeout(BOUND, accept(&listener)).await.unwrap();
        let fresh = trigger(&mut second, "new", "Heartbeat", None, 1, "Accepted").await;
        assert_eq!(fresh[0][2], "Heartbeat");
        second.close(None).await.unwrap();
    });
    let mut config = config(endpoint);
    config.reconnect = true;
    config.trigger_observation.delay_ms = 1_500;
    let client = SimulatorProtocolClient::connect(config).await.unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn triggered_meter_uses_current_reading_without_transaction_identity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let boot = recv(&mut socket).await;
        assert_eq!(boot[2], "BootNotification");
        socket
            .send(Message::text(
                json!([3,boot[1],{
                    "currentTime":"2026-09-01T00:00:00Z","interval":60,"status":"Accepted"
                }])
                .to_string(),
            ))
            .await
            .unwrap();
        let reading = recv(&mut socket).await;
        assert_eq!(reading[2], "MeterValues");
        socket
            .send(Message::text(json!([3, reading[1], {}]).to_string()))
            .await
            .unwrap();
        let second = recv(&mut socket).await;
        assert_eq!(second[2], "MeterValues");
        socket
            .send(Message::text(json!([3, second[1], {}]).to_string()))
            .await
            .unwrap();

        let calls = trigger(&mut socket, "latest", "MeterValues", None, 2, "Accepted").await;
        assert_eq!(
            calls
                .iter()
                .map(|call| call[3]["connectorId"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        for (meter, reading) in calls.iter().zip(["1250", "2500"]) {
            assert_eq!(
                meter[3]["meterValue"][0]["sampledValue"][0]["value"],
                reading
            );
            assert_eq!(
                meter[3]["meterValue"][0]["sampledValue"][0]["context"],
                "Trigger"
            );
            assert!(meter[3].get("transactionId").is_none());
        }
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Simulator"}),
        })
        .await
        .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::MeterValues,
            payload: json!({"connectorId":1,"transactionId":42,"meterValue":[{
                "timestamp":"2026-09-01T00:05:02Z","sampledValue":[{
                    "value":"1250","context":"Sample.Periodic","unit":"Wh"
                }]
            }]}),
        })
        .await
        .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::MeterValues,
            payload: json!({"connectorId":2,"meterValue":[{
                "timestamp":"2026-09-01T00:05:03Z","sampledValue":[{
                    "value":"2500","context":"Sample.Periodic","unit":"Wh"
                }]
            }]}),
        })
        .await
        .unwrap();
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn boot_trigger_remains_available_while_registration_is_pending() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let initial = recv(&mut socket).await;
        assert_eq!(initial[2], "BootNotification");
        socket
            .send(Message::text(
                json!([3,initial[1],{
                    "currentTime":"2026-09-01T00:00:00Z","interval":60,"status":"Pending"
                }])
                .to_string(),
            ))
            .await
            .unwrap();
        let triggered = trigger(
            &mut socket,
            "pending",
            "BootNotification",
            None,
            1,
            "Accepted",
        )
        .await;
        assert_eq!(triggered[0][3]["chargePointVendor"], "UOB");
        trigger(
            &mut socket,
            "accepted",
            "BootNotification",
            None,
            0,
            "Rejected",
        )
        .await;
        socket.close(None).await.unwrap();
    });
    let client = SimulatorProtocolClient::connect(config(endpoint))
        .await
        .unwrap();
    let pending = client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Simulator"}),
        })
        .await
        .unwrap();
    assert_eq!(pending["status"], "Pending");
    timeout(BOUND, server).await.unwrap().unwrap();
    client.shutdown().await.unwrap();
}
