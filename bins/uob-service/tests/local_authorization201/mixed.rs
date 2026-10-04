use super::support::*;
use serde_json::{Value, json};
use std::fs;

#[tokio::test]
async fn mixed_roster_routes_exact_typed_providers_and_preserves_historical_sixteen_versions() {
    let fixture = fixture();
    let config = fixture.root.join("bridge.toml");
    let text = fs::read_to_string(&config).unwrap().replace("id='station-b'\nprotocol='ocpp16j'", "id='station-b'\nprotocol='ocpp16j'\nget_local_list_version=true\nsend_local_list=true\nclear_cache=true");
    fs::write(config, text).unwrap();
    let legacy_reference = format!("list16:{}", "b".repeat(64));
    let legacy_native = json!({"listVersion":-2,"updateType":"Full","localAuthorizationList":[{"idTag":"PRIVATE-SIXTEEN-112","idTagInfo":{"status":"Accepted"}}]});
    save(
        &fixture,
        &json!({"updates":[
            {"station_id":"station-a","update_reference":reference(1),"expires_at":"2099-01-01T00:00:00Z","request":native(1)},
            {"station_id":"station-b","update_reference":legacy_reference,"expires_at":"2099-01-01T00:00:00Z","request":legacy_native}
        ]}),
    );
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut native201 = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut native201).await;
    let mut native16 = fixture.station("station-b").await;
    crate::host::boot(&mut native16).await;
    fixture.connected(&client, "station-a").await;
    fixture.connected(&client, "station-b").await;
    let pending201 = begin(
        &client,
        &fixture,
        command("mixed-native201", "SendLocalList", 1),
    );
    assert_eq!(receive(&mut native201).await[3], native(1));
    send(
        &mut native201,
        json!([3,"mixed-native201",{"status":"Accepted"}]),
    )
    .await;
    let result201 = pending201.await.unwrap().json::<Value>().await.unwrap()["result"].clone();
    assert_eq!(result201["schema_version"]["revision"], 10);
    assert!(result201["local_authorization_16"].is_null());
    let body = json!({"request_id":"mixed-native16","resource":{"bridge_id":"bridge-1","station_id":"station-b"},"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp16j","action":"SendLocalList","payload_schema":"urn:uob:ocpp16:SendLocalListReference:1","payload":{"listVersion":-2,"updateType":"Full","updateReference":legacy_reference}}},"expires_at":"2099-01-01T00:00:00Z"});
    let pending16 = begin(&client, &fixture, body);
    assert_eq!(receive(&mut native16).await[3], legacy_native);
    send(
        &mut native16,
        json!([3,"mixed-native16",{"status":"Accepted"}]),
    )
    .await;
    let result16 = pending16.await.unwrap().json::<Value>().await.unwrap()["result"].clone();
    assert_eq!(result16["schema_version"]["revision"], 9);
    assert_eq!(result16["local_authorization_16"]["list_version"], -2);
    assert!(result16["local_authorization_201"].is_null());
    assert_eq!(result(&client, &fixture, "mixed-native16").await, result16);
    assert_eq!(
        result(&client, &fixture, "mixed-native201").await,
        result201
    );
    assert_private(&fixture, &result201);
    for suffix in ["charging.sqlite3", "charging.sqlite3-wal"] {
        if let Ok(bytes) = fs::read(fixture.root.join("state").join(suffix)) {
            assert!(
                !bytes
                    .windows("PRIVATE-SIXTEEN-112".len())
                    .any(|window| window == b"PRIVATE-SIXTEEN-112")
            );
        }
    }
    no_call(&mut native201).await;
    no_call(&mut native16).await;
}
