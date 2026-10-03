pub(super) use crate::host::{
    CONTROL, Fixture, PRIVILEGED, Socket, client, no_call, receive, result, send, station_call,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

pub(super) const SECRET: &str = "PRIVATE201-MARKER";
pub(super) fn reference(index: usize) -> String {
    format!("cfg201:{index:064x}")
}
pub(super) fn resource() -> Value {
    json!({"bridge_id":"bridge-1","station_id":"station-a"})
}
pub(super) fn entry(index: usize, name: &str) -> Value {
    json!({"component":{"name":"VendorCtrlr"},"variable":{"name":name},"valueReference":reference(index)})
}
pub(super) fn profile(url: &str) -> Value {
    json!({"ocppVersion":"OCPP20","ocppTransport":"JSON","ocppCsmsUrl":url,
        "messageTimeout":30,"securityProfile":1,"ocppInterface":"Wired0",
        "apn":{"apn":"synthetic","apnUserName":"user","apnPassword":SECRET,
            "apnAuthentication":"PAP","simPin":0,"preferredNetwork":"20404","useOnlyPreferredNetwork":false},
        "vpn":{"server":"vpn.invalid","user":"synthetic","password":SECRET,"key":SECRET,"type":"IKEv2"}})
}
pub(super) fn provisioning() -> Value {
    let names = [
        "Empty",
        "Unicode",
        "ReadOnly",
        "Range",
        "UnknownComponent",
        "UnknownVariable",
        "Unsupported",
        "Reboot",
        "Password",
        "Expired",
    ];
    let variables: Vec<Value> = names.iter().enumerate().map(|(i, name)| {
        let value = match *name {
            "Empty" => String::new(),
            "Unicode" => "🦀".repeat(1000),
            "Range" => "101".to_owned(),
            _ => SECRET.to_owned(),
        };
        json!({"resource":resource(),"entry":entry(i + 1,name),"value":value,
            "expires_at":if *name == "Expired" {"2000-01-01T00:00:00Z"} else {"2099-01-01T00:00:00Z"}})
    }).collect();
    let network_profiles: Vec<Value> = (0..3).map(|slot| {
        let index = usize::try_from(slot).expect("fixture network slot is nonnegative");
        json!({"resource":resource(),"configuration_slot":slot,"reference":reference(20 + index),
            "profile":profile(&format!("wss://csms-{slot}.invalid/ocpp")),"expires_at":"2099-01-01T00:00:00Z"})
    }).collect();
    json!({"variables":variables,"network_profiles":network_profiles})
}
pub(super) fn save_provisioning(fixture: &Fixture, value: &Value) {
    let temporary = fixture.root.join("configuration201-next.json");
    fs::write(&temporary, value.to_string()).unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(temporary, fixture.root.join("configuration201.json")).unwrap();
}
pub(super) fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replace("protocol='ocpp16j'", "protocol='ocpp201'")
        .replace(
            "get_composite_schedule=true",
            "set_variables=true\nset_network_profile=true\nget_variables=true",
        )
        .replace(
            "[[charging.stations.resources]]\n",
            "[[charging.stations.resources]]\nevse_id='evse-one'\n",
        )
        .replace(
            "native_connector_id=",
            "native_evse_id=1\nnative_connector_id=",
        )
        .replace(
            "[charging]\n",
            &format!(
                "[charging]\nconfiguration_values_file='{}'\n",
                fixture.root.join("configuration201.json").display()
            ),
        );
    fs::write(path, config).unwrap();
    save_provisioning(&fixture, &provisioning());
    fixture
}
pub(super) async fn boot(socket: &mut Socket) {
    let reply = station_call(socket,"boot","BootNotification",json!({"chargingStation":{"vendorName":"Independent","model":"ConfigurationPeer"},"reason":"PowerUp"})).await;
    assert_eq!(reply["status"], "Accepted");
}
pub(super) fn command(id: &str, action: &str, payload: Value) -> Value {
    let schema = match action {
        "SetVariables" => "urn:uob:ocpp201:SetVariablesReference:1".to_owned(),
        "SetNetworkProfile" => "urn:uob:ocpp201:SetNetworkProfileReference:1".to_owned(),
        _ => format!("urn:OCPP:Cp:2:2020:3:{action}Request"),
    };
    let mut command = json!({"request_id":id,"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp201","action":action}},"expires_at":"2099-01-01T00:00:00Z"});
    command
        .as_object_mut()
        .unwrap()
        .insert("resource".to_owned(), resource());
    let parameters = command["operation"]["parameters"].as_object_mut().unwrap();
    parameters.insert("payload_schema".to_owned(), Value::String(schema));
    parameters.insert("payload".to_owned(), payload);
    command
}
pub(super) fn variables(id: &str, entries: Vec<Value>) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert("setVariableData".to_owned(), Value::Array(entries));
    command(id, "SetVariables", Value::Object(payload))
}
pub(super) fn network(id: &str, slot: i32) -> Value {
    let index = usize::try_from(slot).expect("fixture network slot is nonnegative");
    command(
        id,
        "SetNetworkProfile",
        json!({"configurationSlot":slot,"profileReference":reference(20 + index)}),
    )
}

// Independent station policy: results depend on metadata/range, never the bridge encoder.
#[derive(Default)]
pub(super) struct Peer {
    pub values: BTreeMap<String, String>,
    pub staged: BTreeMap<i32, Value>,
    pub active: Option<Value>,
}
impl Peer {
    pub fn set_variables(&mut self, payload: &Value) -> Value {
        let mut replies = Vec::new();
        for item in payload["setVariableData"].as_array().unwrap() {
            let name = item["variable"]["name"].as_str().unwrap();
            let value = item["attributeValue"].as_str().unwrap();
            let status = match name {
                "ReadOnly" => "Rejected",
                "Range" if value.parse::<u32>().unwrap() > 100 => "Rejected",
                "UnknownComponent" => "UnknownComponent",
                "UnknownVariable" => "UnknownVariable",
                "Unsupported" => "NotSupportedAttributeType",
                "Reboot" => "RebootRequired",
                _ => "Accepted",
            };
            if status == "Accepted" || status == "RebootRequired" {
                self.values.insert(name.to_owned(), value.to_owned());
            }
            replies.push(json!({"component":item["component"],"variable":item["variable"],
                "attributeType":item.get("attributeType").cloned().unwrap_or(json!("Actual")),"attributeStatus":status}));
        }
        replies.reverse();
        json!({"setVariableResult":replies})
    }
    pub fn set_network(&mut self, payload: &Value) -> Value {
        let slot = i32::try_from(payload["configurationSlot"].as_i64().unwrap())
            .expect("native fixture configuration slot fits signed32-bit");
        let status = match slot {
            1 => "Rejected",
            2 => "Failed",
            _ => "Accepted",
        };
        if status == "Accepted" {
            self.staged.insert(slot, payload["connectionData"].clone());
        }
        json!({"status":status})
    }
    pub fn reboot(&mut self, slot: i32) {
        self.active = self.staged.get(&slot).cloned();
    }
}
pub(super) async fn learn_limits(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    count: &str,
    bytes: &str,
) {
    for (id, name, value) in [
        ("learn-items", "ItemsPerMessage", count),
        ("learn-bytes", "BytesPerMessage", bytes),
    ] {
        let identity = json!({"component":{"name":"DeviceDataCtrlr"},"variable":{"name":name,"instance":"SetVariables"}});
        let request = begin(
            client,
            fixture,
            command(id, "GetVariables", json!({"getVariableData":[identity]})),
        );
        assert_eq!(
            receive(socket).await,
            json!([2,id,"GetVariables",{"getVariableData":[identity]}])
        );
        let mut reply = identity;
        reply["attributeStatus"] = json!("Accepted");
        reply["attributeValue"] = json!(value);
        send(socket, json!([3,id,{"getVariableResult":[reply]}])).await;
        let response = request.task.await.unwrap();
        let status = response.status();
        let response: Value = response.json().await.unwrap();
        assert_eq!(
            status, 202,
            "explicit query {id}: error={}",
            response["error"]
        );
        assert_eq!(
            response["result"]["device_model_201"]["variables"][0]["value"]["value"],
            value
        );
    }
}
pub(super) fn assert_no_secrets(fixture: &Fixture, result: &Value) {
    assert!(!result.to_string().contains(SECRET));
    assert!(!result.to_string().contains("cfg201:"));
    for file in ["charging.sqlite3", "charging.sqlite3-wal"] {
        let path = fixture.root.join("state").join(file);
        if path.exists() {
            let bytes = fs::read(path).unwrap();
            assert!(
                !bytes
                    .windows(SECRET.len())
                    .any(|window| window == SECRET.as_bytes())
            );
        }
    }
}

pub(super) async fn assert_not_admitted(client: &reqwest::Client, fixture: &Fixture, body: Value) {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let response = client
        .get(fixture.url(&format!(
            "/api/v1/commands/{}",
            body["request_id"].as_str().unwrap()
        )))
        .bearer_auth(crate::host::READ)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        404,
        "reference rejection precedes durable admission"
    );
}

pub(super) struct PendingCommand {
    id: String,
    task: crate::host::Pending,
}

impl PendingCommand {
    pub(super) fn abort(&self) {
        self.task.abort();
    }
}

pub(super) fn begin(client: &reqwest::Client, fixture: &Fixture, body: Value) -> PendingCommand {
    PendingCommand {
        id: body["request_id"].as_str().unwrap().to_owned(),
        task: crate::host::begin(client, fixture, body),
    }
}

pub(super) async fn completed(request: PendingCommand) -> Value {
    let response = request.task.await.unwrap();
    let status = response.status();
    let response: Value = response.json().await.unwrap();
    assert_eq!(
        status, 202,
        "command {}: error={}, return_route={}",
        request.id, response["error"], response["result"]["return_route"]
    );
    response["result"].clone()
}

pub(super) async fn known_not_sent(
    client: &reqwest::Client,
    fixture: &Fixture,
    request: PendingCommand,
) -> Value {
    let response = request.task.await.unwrap();
    assert_eq!(
        response.status(),
        400,
        "known-not-sent command {}",
        request.id
    );
    let rejected: Value = response.json().await.unwrap();
    assert_eq!(rejected["return_route"]["request_id"], request.id);
    assert_eq!(rejected["lifecycle"]["stage"], "rejected");
    assert_eq!(rejected["lifecycle"]["error"]["code"], "invalid_parameters");
    // A flat HTTP rejection is distinct from a pre-admission {error} response:
    // actual dispatch admission exists and its durable final outcome is definitely no send.
    assert_eq!(result(client, fixture, &request.id).await, rejected);
    assert_no_secrets(fixture, &rejected);
    rejected
}
