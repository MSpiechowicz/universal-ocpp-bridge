pub(super) use crate::host::{
    CONTROL, Fixture, PRIVILEGED, Socket, begin, client, completed, no_call, receive, result, send,
    station_call,
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use std::{fs, time::Duration};

/// Station A opts into K08/K09 with a grid scope, EVSE 1 and its connector; station B does not.
pub(super) fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path).unwrap();
    let (head, _) = config.split_once("[[charging.stations]]").unwrap();
    let credential = |name: &str| fixture.root.join(name).display().to_string();
    let stations = format!(
        "[[charging.stations]]\nid='station-a'\nprotocol='ocpp201'\nget_composite_schedule=true\n\
         get_charging_profiles=true\ncredential_file='{}'\n\
         [[charging.stations.resources]]\nevse_id='evse-one'\nnative_evse_id=1\n\
         [[charging.stations.resources]]\nevse_id='evse-one'\nconnector_id='one'\nnative_evse_id=1\nnative_connector_id=1\n\
         [[charging.stations]]\nid='station-b'\nprotocol='ocpp201'\ncredential_file='{}'\n\
         [[charging.stations.resources]]\nevse_id='evse-one'\nnative_evse_id=1\n",
        credential("station-a"),
        credential("station-b"),
    );
    fs::write(path, format!("{head}{stations}")).unwrap();
    fixture
}

pub(super) async fn boot(socket: &mut Socket) {
    let reply = station_call(
        socket,
        "boot",
        "BootNotification",
        json!({"chargingStation":{"vendorName":"Independent","model":"SchedulePeer"},"reason":"PowerUp"}),
    )
    .await;
    assert_eq!(reply["status"], "Accepted");
}

/// Station scope addresses the grid connection; `evse` addresses native EVSE 1.
pub(super) fn command(id: &str, action: &str, payload: Value, evse: bool) -> Value {
    let mut resource = json!({"bridge_id":"bridge-1","station_id":"station-a"});
    if evse {
        resource["resource"] = json!({"kind":"evse","evse_id":"evse-one"});
        resource["native_protocol_reference"] = json!({"protocol":"ocpp201","evse_id":1});
    }
    let mut command = json!({"request_id":id,"resource":resource,"operation":{"kind":"ocpp",
        "parameters":{"protocol":"ocpp201","action":action,
        "payload_schema":format!("urn:OCPP:Cp:2:2020:3:{action}Request")}},
        "expires_at":"2099-01-01T00:00:00Z"});
    command["operation"]["parameters"]["payload"] = payload;
    command
}

/// Answers the next native CALL, which must carry exactly the submitted payload.
pub(super) async fn answer(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: &Value,
    reply: &str,
) -> Value {
    let request = begin(client, fixture, body.clone());
    let call = receive(socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[2], body["operation"]["parameters"]["action"]);
    assert_eq!(call[3], body["operation"]["parameters"]["payload"]);
    // Exact numeric lexemes are written verbatim, never through a binary float.
    let frame = format!("[3,{},{reply}]", call[1]);
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(frame.into()))
        .await
        .unwrap();
    completed(request).await
}

pub(super) async fn report(socket: &mut Socket, id: &str, payload: Value) -> Value {
    send(socket, json!([2, id, "ReportChargingProfiles", payload])).await;
    receive(socket).await
}

pub(super) async fn terminal(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let value = result(client, fixture, id).await;
            if value["charging_profiles_201"]["report"]["state"] != "pending" {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("terminal report status")
}

pub(super) async fn status(
    client: &reqwest::Client,
    fixture: &Fixture,
    token: &str,
    body: &Value,
) -> u16 {
    client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}
