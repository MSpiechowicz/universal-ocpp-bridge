pub(super) use crate::host::{
    CONTROL, Fixture, Socket, client, completed, no_call, receive, send, station_call,
};
use serde_json::{Value, json};
use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Station A lets its EMS answer charging needs through canonical limits; station B keeps the
/// default policy. Both expose EVSE 1 and its connector over OCPP 2.0.1.
pub(super) fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path).unwrap();
    let (head, _) = config.split_once("[[charging.stations]]").unwrap();
    let credential = |name: &str| fixture.root.join(name).display().to_string();
    let resources = "[[charging.stations.resources]]\nevse_id='evse-one'\nnative_evse_id=1\n\
         [[charging.stations.resources]]\nevse_id='evse-one'\nconnector_id='one'\nnative_evse_id=1\nnative_connector_id=1\n";
    let stations = format!(
        "[[charging.stations]]\nid='station-a'\nprotocol='ocpp201'\nallow_charging_limit=true\n\
         ev_charging_needs_processing=true\ncredential_file='{}'\n{resources}\
         [[charging.stations]]\nid='station-b'\nprotocol='ocpp201'\ncredential_file='{}'\n{resources}",
        credential("station-a"),
        credential("station-b"),
    );
    fs::write(path, format!("{head}{stations}")).unwrap();
    fixture
}

pub(super) async fn connect(fixture: &Fixture, client: &reqwest::Client, station: &str) -> Socket {
    let mut socket = fixture.station_protocol(station, "ocpp2.0.1").await;
    fixture.connected(client, station).await;
    let reply = station_call(
        &mut socket,
        "boot",
        "BootNotification",
        json!({"chargingStation":{"vendorName":"Independent","model":"NegotiationPeer"},
            "reason":"PowerUp"}),
    )
    .await;
    assert_eq!(reply["status"], "Accepted");
    socket
}

pub(super) async fn started(socket: &mut Socket, transaction: &str) {
    let timestamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let reply = station_call(
        socket,
        &format!("start-{transaction}"),
        "TransactionEvent",
        json!({"eventType":"Started","timestamp":timestamp,"triggerReason":"CablePluggedIn",
            "seqNo":0,"evse":{"id":1,"connectorId":1},
            "transactionInfo":{"transactionId":transaction,"chargingState":"Charging"}}),
    )
    .await;
    assert_eq!(reply, json!({}));
}

/// Sends one independently authored corpus CALL, optionally edited, and returns the reply.
/// OCPP-J message IDs are unique per sender, so each send gets a fresh one.
pub(super) async fn notify(
    socket: &mut Socket,
    fixture: &str,
    edit: impl FnOnce(&mut Value),
) -> Value {
    static SENT: AtomicUsize = AtomicUsize::new(0);
    let mut frame = corpus(fixture);
    let id = format!(
        "{}-{}",
        frame[1].as_str().unwrap(),
        SENT.fetch_add(1, Ordering::Relaxed)
    );
    frame[1] = json!(id);
    edit(&mut frame[3]);
    send(socket, frame.clone()).await;
    let reply = receive(socket).await;
    assert_eq!(
        reply[1], frame[1],
        "the answer correlates to the station's CALL"
    );
    reply
}

pub(super) fn corpus(name: &str) -> Value {
    serde_json::from_slice(
        &fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/ocpp-fixtures/corpus/wire/2.0.1")
                .join(format!("{name}.json")),
        )
        .unwrap(),
    )
    .unwrap()
}

/// A typed point on the exact EVSE 1 resource, or on the station when `evse` is false.
pub(super) fn point(snapshot: &Value, evse: bool, id: &str) -> Value {
    let values = if evse {
        snapshot["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|resource| {
                let native = &resource["resource"]["native_protocol_reference"];
                native["evse_id"] == 1 && native.get("connector_id").is_none_or(Value::is_null)
            })
            .map_or(Value::Null, |resource| resource["current_values"].clone())
    } else {
        snapshot["current_values"].clone()
    };
    values
        .as_array()
        .and_then(|values| values.iter().find(|value| value["point_id"] == id))
        .map_or(Value::Null, |value| value["value"].clone())
}

pub(super) fn text(value: &str) -> Value {
    json!({"type":"text","value":value})
}

/// Canonical EVSE 1 charging limit through the ordinary control path.
pub(super) fn limit(id: &str, amperes: &str) -> Value {
    json!({"request_id":id,"expires_at":"2099-01-01T00:00:00Z",
        "resource":{"bridge_id":"bridge-1","station_id":"station-a",
            "resource":{"kind":"evse","evse_id":"evse-one"},
            "native_protocol_reference":{"protocol":"ocpp201","evse_id":1}},
        "operation":{"kind":"set_charging_limit",
            "parameters":{"value":amperes,"unit":"ampere","phases":3}}})
}

pub(super) async fn installed(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
) -> Value {
    let worker = client.clone();
    let url = fixture.url("/api/v1/commands");
    let pending = tokio::spawn(async move {
        worker
            .post(url)
            .bearer_auth(CONTROL)
            .json(&body)
            .send()
            .await
            .unwrap()
    });
    let call = receive(socket).await;
    assert_eq!(call[2], "SetChargingProfile");
    send(socket, json!([3, call[1], {"status":"Accepted"}])).await;
    let result = completed(pending).await;
    assert_eq!(result["lifecycle"]["accepted"], true);
    call
}
