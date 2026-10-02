pub use super::daemon::{
    CONTROL, PRIVILEGED, READ, Socket, begin, boot, client, completed, no_call, receive, result,
    send, station_call,
};
use serde_json::{Value, json};
use std::{fs, ops::Deref, os::unix::fs::PermissionsExt};

const START_ID_TAG: &str = "PROFILE-TAG-117-AB";

pub struct Fixture(super::daemon::Fixture);
impl Fixture {
    pub fn new() -> Self {
        let fixture = super::daemon::Fixture::new();
        let token = fixture.root.join("start-tag");
        fs::write(&token, START_ID_TAG).unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        let path = fixture.root.join("bridge.toml");
        let configuration = fs::read_to_string(&path).unwrap().replace(
            "get_composite_schedule=true",
            &format!(
                "set_charging_profile=true\nclear_charging_profile=true\nstart_token_file='{}'",
                token.display()
            ),
        );
        fs::write(path, configuration).unwrap();
        Self(fixture)
    }
}
impl Deref for Fixture {
    type Target = super::daemon::Fixture;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub fn profile(connector: u32, id: i32, rate: &str, unit: &str) -> Value {
    let mut value = json!({"connectorId":connector,"csChargingProfiles":{
        "chargingProfileId":id,"stackLevel":2,"chargingProfilePurpose":"TxDefaultProfile",
        "chargingProfileKind":"Recurring","recurrencyKind":"Daily",
        "validFrom":"2026-01-01T00:00:00Z","validTo":"2099-01-01T00:00:00Z",
        "chargingSchedule":{"duration":60,"startSchedule":"2026-01-01T00:00:00Z","chargingRateUnit":unit,
            "chargingSchedulePeriod":[{"startPeriod":0,"limit":0,"numberPhases":4},{"startPeriod":86401,"limit":0}],"minChargingRate":8.1}
    }});
    value["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"] =
        serde_json::from_str(rate).unwrap();
    value
}
pub fn command(id: &str, connector: u32, action: &str, payload: Value) -> Value {
    let mut body = super::daemon::command(id, connector, None);
    let mut parameters = json!({
        "protocol": "ocpp16j",
        "action": action,
        "payload_schema": format!("urn:OCPP:1.6:2019:12:{action}Request"),
    });
    parameters["payload"] = payload;
    body["operation"]["parameters"] = parameters;
    body
}
pub async fn start_transaction(socket: &mut Socket) -> i32 {
    let timestamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let response = station_call(
        socket,
        "native-start",
        "StartTransaction",
        json!({"connectorId":1,"idTag":START_ID_TAG,"meterStart":0,"timestamp":timestamp}),
    )
    .await;
    assert_eq!(response["idTagInfo"]["status"], "Accepted");
    i32::try_from(response["transactionId"].as_i64().unwrap()).unwrap()
}
pub async fn exchange(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    body: Value,
    status: &str,
) -> Value {
    let expected = body["operation"]["parameters"]["action"].clone();
    let pending = begin(client, fixture, body);
    let call = receive(socket).await;
    assert_eq!(call[0], 2);
    assert_eq!(call[2], expected);
    send(socket, json!([3,call[1],{"status":status}])).await;
    let result = completed(pending).await;
    assert_eq!(result["charging_profile_16"]["status"], status);
    assert_eq!(result["charging_profile_16"]["action"], expected);
    result
}
pub async fn disconnected(client: &reqwest::Client, fixture: &Fixture) {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            if fixture.snapshot(client, "station-a").await["connectivity"]["status"]
                == "disconnected"
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("old generation disconnected");
}
