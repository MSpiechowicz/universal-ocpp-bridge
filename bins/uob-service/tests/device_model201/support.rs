pub(super) use crate::host::{
    CONTROL, Fixture, PRIVILEGED, Socket, begin, client, completed, no_call, receive, result, send,
    station_call,
};
use serde_json::{Value, json};
use std::{fs, time::Duration};

pub(super) fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replace("protocol='ocpp16j'", "protocol='ocpp201'")
        .replace(
            "get_composite_schedule=true",
            "get_variables=true\nget_base_report=true\nget_report=true",
        )
        .replace(
            "[[charging.stations.resources]]\n",
            "[[charging.stations.resources]]\nevse_id='evse-one'\n",
        )
        .replace(
            "native_connector_id=",
            "native_evse_id=1\nnative_connector_id=",
        );
    fs::write(path, config).unwrap();
    fixture
}
pub(super) async fn boot(socket: &mut Socket) {
    let reply = station_call(socket, "boot", "BootNotification", json!({"chargingStation":{"vendorName":"Independent","model":"DevicePeer"},"reason":"PowerUp"})).await;
    assert_eq!(reply["status"], "Accepted");
}
pub(super) fn command(id: &str, action: &str, payload: Value) -> Value {
    let mut command = json!({"request_id":id,"resource":{"bridge_id":"bridge-1","station_id":"station-a"},"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":action,"payload_schema":format!("urn:OCPP:Cp:2:2020:3:{action}Request")}},"expires_at":"2099-01-01T00:00:00Z"});
    command["operation"]["parameters"]["payload"] = payload;
    command
}
pub(super) async fn terminal(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let value = result(client, fixture, id).await;
            if value["device_model_201"]["report"]["state"] != "pending" {
                return value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal report status")
}
