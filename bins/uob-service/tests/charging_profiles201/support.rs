pub use super::daemon::{
    CONTROL, PRIVILEGED, begin, client, completed, no_call, receive, result, send, station_call,
};
use super::peer::{Peer, Socket};
use serde_json::{Value, json};
use std::{fs, ops::Deref};

pub struct Fixture(super::daemon::Fixture);
impl Fixture {
    pub fn new(full: bool) -> Self {
        let fixture = super::daemon::Fixture::new();
        let path = fixture.root.join("bridge.toml");
        let old = fs::read_to_string(&path).unwrap();
        let (alpha, _) = old
            .split_once("[[charging.stations]]\nid='station-b'")
            .unwrap();
        let config = format!(
            "{alpha}[[charging.stations]]\nid='station-b'\nprotocol='ocpp201'\ncredential_file='{}'\nset_charging_profile={full}\nclear_charging_profile=true\nget_variables=true\nallow_charging_limit=true\n\
            [[charging.stations.resources]]\nevse_id='one'\nnative_evse_id=1\n\
            [[charging.stations.resources]]\nevse_id='one'\nconnector_id='socket'\nnative_evse_id=1\nnative_connector_id=1\n\
            [[charging.stations.resources]]\nevse_id='two'\nnative_evse_id=2\n",
            fixture.root.join("station-b").display()
        );
        fs::write(path, config).unwrap();
        Self(fixture)
    }
    pub fn peer(&self) -> Peer {
        Peer::open(self.root.join("peer-state.json")).unwrap()
    }
    pub async fn station201(&self) -> Socket {
        self.station_protocol("station-b", "ocpp2.0.1").await
    }
}
impl Deref for Fixture {
    type Target = super::daemon::Fixture;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
pub fn resource(evse: u32) -> Value {
    let mut resource = json!({"bridge_id":"bridge-1","station_id":"station-b"});
    if evse > 0 {
        resource["resource"] =
            json!({"kind":"evse","evse_id":if evse == 1 { "one" } else { "two" }});
        resource["native_protocol_reference"] = json!({"protocol":"ocpp201","evse_id":evse});
    }
    resource
}
pub fn command(id: &str, evse: u32, action: &str, payload: Value) -> Value {
    let mut envelope = json!({"request_id":id,"resource":resource(evse),"expires_at":"2099-01-01T00:00:00Z",
        "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":action,
            "payload_schema":format!("urn:OCPP:Cp:2:2020:3:{action}Request")}}});
    envelope["operation"]["parameters"]
        .as_object_mut()
        .unwrap()
        .insert("payload".to_owned(), payload);
    envelope
}
pub fn profile(id: i32, rate: &str) -> Value {
    let mut payload = json!({"evseId":1,"chargingProfile":{"id":id,"stackLevel":2,
        "chargingProfilePurpose":"TxDefaultProfile","chargingProfileKind":"Absolute",
        "validFrom":"2026-01-01T00:00:00Z","validTo":"2099-01-01T00:00:00Z",
        "chargingSchedule":[{"id":-118,"startSchedule":"2026-01-01T00:00:00Z","duration":60,
            "chargingRateUnit":"W","chargingSchedulePeriod":[{"startPeriod":0,"limit":0},
                {"startPeriod":120,"limit":0}],"minChargingRate":0}]}});
    payload["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"] =
        serde_json::from_str(rate).unwrap();
    payload
}
pub fn phase_query(id: &str) -> Value {
    command(
        id,
        1,
        "GetVariables",
        json!({"getVariableData":[{"component":{"name":"SmartChargingCtrlr","evse":{"id":1}},
        "variable":{"name":"ACPhaseSwitchingSupported"},"attributeType":"Actual"}]}),
    )
}
pub async fn exchange(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    peer: &mut Peer,
    body: Value,
) -> Value {
    let pending = begin(client, fixture, body);
    peer.serve_one(socket).await.unwrap();
    completed(pending).await
}
pub async fn baseline(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    peer: &mut Peer,
) {
    for (index, purpose) in ["ChargingStationMaxProfile", "TxDefaultProfile", "TxProfile"]
        .into_iter()
        .enumerate()
    {
        let result = exchange(
            client,
            fixture,
            socket,
            peer,
            command(
                &format!("baseline-{index}"),
                0,
                "ClearChargingProfile",
                json!({"chargingProfileCriteria":{"chargingProfilePurpose":purpose}}),
            ),
        )
        .await;
        assert_eq!(result["charging_profile_201"]["status"], "Unknown");
    }
}
pub async fn denied(client: &reqwest::Client, fixture: &Fixture, body: Value) -> Value {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&body)
        .send()
        .await
        .unwrap();
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["lifecycle"]["stage"], "rejected");
    assert!(value.get("charging_profile_201").is_none());
    value
}
pub async fn disconnected(client: &reqwest::Client, fixture: &Fixture) {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            if fixture.snapshot(client, "station-b").await["connectivity"]["status"]
                == "disconnected"
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}

pub async fn start_peer(
    fixture: &Fixture,
) -> (super::daemon::Process, reqwest::Client, Socket, Peer) {
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station201().await;
    fixture.connected(&client, "station-b").await;
    let peer = fixture.peer();
    assert_eq!(Peer::boot(&mut socket).await.unwrap()["status"], "Accepted");
    (process, client, socket, peer)
}

pub async fn assert_replay_without_call(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: &Value,
    expected: &Value,
) {
    let duplicate = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        &duplicate.json::<Value>().await.unwrap()["result"],
        expected
    );
    no_call(socket).await;
}
