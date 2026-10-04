pub use crate::host::{CONTROL, Fixture, PRIVILEGED, READ, client, no_call, receive, result, send};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};
pub const TOKEN: &str = "PRIVATE-LIST-112";
pub const PARENT: &str = "PRIVATE-PARENT-112";
pub const MESSAGE: &str = "PRIVATE-MESSAGE-112";
pub const EXTRA: &str = "PRIVATE-EXTRA-112";
pub const VENDOR: &str = "PRIVATE-VENDOR-112";
pub const REPLY: &str = "PRIVATE-REPLY-112";
pub fn reference(index: usize) -> String {
    format!("list201:{index:064x}")
}
pub fn native(index: usize) -> Value {
    match index {
        1 => {
            json!({"versionNumber":1,"updateType":"Full","localAuthorizationList":[{
                "idToken":{"idToken":TOKEN,"type":"Local","additionalInfo":[{"additionalIdToken":EXTRA,"type":"vendor-UTF8-ż"}]},
                "idTokenInfo":{"status":"Accepted","groupIdToken":{"idToken":PARENT,"type":"Local"},
                    "cacheExpiryDateTime":"2099-01-01T00:00:00Z",
                    "personalMessage":{"format":"UTF8","content":MESSAGE},
                    "customData":{"vendorId":"vendor","private":VENDOR}}
            }]})
        }
        2 => {
            json!({"versionNumber":2,"updateType":"Differential","localAuthorizationList":[{"idToken":{"idToken":TOKEN,"type":"Local"}}]})
        }
        3 => json!({"versionNumber":3,"updateType":"Full"}),
        _ => json!({"versionNumber":4,"updateType":"Differential"}),
    }
}
pub fn save(fixture: &Fixture, value: &Value) {
    let temporary = fixture.root.join("updates-next.json");
    fs::write(&temporary, value.to_string()).unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(temporary, fixture.root.join("updates.json")).unwrap();
}
pub fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replace(
            "get_composite_schedule=true",
            "get_local_list_version=true\nsend_local_list=true\nclear_cache=true",
        )
        .replace(
            "[charging]\n",
            &format!(
                "[charging]\nlocal_authorization_updates_file='{}'\n",
                fixture.root.join("updates.json").display()
            ),
        );
    let config = config
        .replacen("protocol='ocpp16j'", "protocol='ocpp201'", 1)
        .replacen(
            "connector_id='one'\nnative_connector_id=1",
            "evse_id='one'\nnative_evse_id=1\nconnector_id='one'\nnative_connector_id=1",
            1,
        )
        .replacen(
            "connector_id='two'\nnative_connector_id=2",
            "evse_id='two'\nnative_evse_id=2\nconnector_id='one'\nnative_connector_id=1",
            1,
        );
    fs::write(path, config).unwrap();
    let updates: Vec<Value> = (1..=4).map(|index| json!({"station_id":"station-a","update_reference":reference(index),"expires_at":"2099-01-01T00:00:00Z","request":native(index)})).collect();
    save(&fixture, &json!({"updates":updates}));
    fixture
}
pub fn command(id: &str, action: &str, index: usize) -> Value {
    let schema = if action == "SendLocalList" {
        "urn:uob:ocpp201:SendLocalListReference:1".to_owned()
    } else {
        format!("urn:OCPP:Cp:2:2020:3:{action}Request")
    };
    let payload = if action == "SendLocalList" {
        json!({"versionNumber":native(index)["versionNumber"],"updateType":native(index)["updateType"],"updateReference":reference(index)})
    } else {
        json!({})
    };
    json!({"request_id":id,"resource":{"bridge_id":"bridge-1","station_id":"station-a"},"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":action,"payload_schema":schema,"payload":payload}},"expires_at":"2099-01-01T00:00:00Z"})
}
pub fn begin(
    client: &reqwest::Client,
    fixture: &Fixture,
    body: Value,
) -> tokio::task::JoinHandle<reqwest::Response> {
    let client = client.clone();
    let url = fixture.url("/api/v1/commands");
    tokio::spawn(async move {
        client
            .post(url)
            .bearer_auth(PRIVILEGED)
            .json(&body)
            .send()
            .await
            .unwrap()
    })
}
pub fn assert_private(fixture: &Fixture, public: &Value) {
    for marker in [TOKEN, PARENT, MESSAGE, EXTRA, VENDOR, REPLY, "list201:"] {
        assert!(!public.to_string().contains(marker));
    }
    for file in ["charging.sqlite3", "charging.sqlite3-wal"] {
        if let Ok(bytes) = fs::read(fixture.root.join("state").join(file)) {
            for marker in [TOKEN, PARENT, MESSAGE, EXTRA, VENDOR, REPLY] {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes())
                );
            }
        }
    }
}

pub async fn boot(socket: &mut crate::host::Socket) {
    send(socket, json!([2,"boot","BootNotification",{"chargingStation":{"model":"model","vendorName":"vendor"},"reason":"PowerUp"}])).await;
    assert_eq!(receive(socket).await[2]["status"], "Accepted");
}
