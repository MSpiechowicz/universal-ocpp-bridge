pub use crate::host::{
    CONTROL, Fixture, PRIVILEGED, READ, boot, client, no_call, receive, result, send,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};
pub const TOKEN: &str = "PRIVATE-LIST-111";
pub const PARENT: &str = "PRIVATE-PARENT-111";
pub fn reference(index: usize) -> String {
    format!("list16:{index:064x}")
}
pub fn native(index: usize) -> Value {
    match index {
        1 => {
            json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[{"idTag":TOKEN,"idTagInfo":{"status":"Accepted","parentIdTag":PARENT,"expiryDate":"2099-01-01T00:00:00Z"}}]})
        }
        2 => {
            json!({"listVersion":2,"updateType":"Differential","localAuthorizationList":[{"idTag":TOKEN}]})
        }
        3 => json!({"listVersion":-2,"updateType":"Full"}),
        _ => json!({"listVersion":4,"updateType":"Differential","localAuthorizationList":[]}),
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
    fs::write(path, config).unwrap();
    let updates: Vec<Value> = (1..=4).map(|index| json!({"station_id":"station-a","update_reference":reference(index),"expires_at":"2099-01-01T00:00:00Z","request":native(index)})).collect();
    save(&fixture, &json!({"updates":updates}));
    fixture
}
pub fn command(id: &str, action: &str, index: usize) -> Value {
    let schema = if action == "SendLocalList" {
        "urn:uob:ocpp16:SendLocalListReference:1".to_owned()
    } else {
        format!("urn:OCPP:1.6:2019:12:{action}Request")
    };
    let payload = if action == "SendLocalList" {
        json!({"listVersion":native(index)["listVersion"],"updateType":native(index)["updateType"],"updateReference":reference(index)})
    } else {
        json!({})
    };
    json!({"request_id":id,"resource":{"bridge_id":"bridge-1","station_id":"station-a"},"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp16j","action":action,"payload_schema":schema,"payload":payload}},"expires_at":"2099-01-01T00:00:00Z"})
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
    for marker in [TOKEN, PARENT, "list16:"] {
        assert!(!public.to_string().contains(marker));
    }
    for file in ["charging.sqlite3", "charging.sqlite3-wal"] {
        if let Ok(bytes) = fs::read(fixture.root.join("state").join(file)) {
            for marker in [TOKEN, PARENT] {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes())
                );
            }
        }
    }
}
