use super::support::*;
use serde_json::{Value, json};
use std::fs;

async fn learn(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut crate::host::Socket,
    id: &str,
    name: &str,
    value: &str,
) {
    let identity = json!({"component":{"name":"LocalAuthListCtrlr"},"variable":{"name":name}});
    let body = json!({"request_id":id,"resource":{"bridge_id":"bridge-1","station_id":"station-a"},"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":"GetVariables","payload_schema":"urn:OCPP:Cp:2:2020:3:GetVariablesRequest","payload":{"getVariableData":[identity]}}},"expires_at":"2099-01-01T00:00:00Z"});
    let pending = begin(client, fixture, body);
    assert_eq!(receive(socket).await[2], "GetVariables");
    send(socket, json!([3,id,{"getVariableResult":[{"component":identity["component"],"variable":identity["variable"],"attributeStatus":"Accepted","attributeValue":value}]}])).await;
    assert_eq!(pending.await.unwrap().status(), 202);
}
#[tokio::test]
async fn correlated_native_request_limits_keep_count_distinct_and_reset_on_new_socket() {
    let fixture = fixture();
    let config = fixture.root.join("bridge.toml");
    fs::write(
        &config,
        fs::read_to_string(&config).unwrap().replace(
            "get_local_list_version=true",
            "get_local_list_version=true\nget_variables=true",
        ),
    )
    .unwrap();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    learn(
        &client,
        &fixture,
        &mut socket,
        "entries-zero",
        "Entries",
        "0",
    )
    .await;
    let accepted = begin(
        &client,
        &fixture,
        command("full-after-count-zero", "SendLocalList", 1),
    );
    assert_eq!(receive(&mut socket).await[3], native(1));
    send(&mut socket, json!([3,"full-after-count-zero",{"status":"Accepted","statusInfo":{"reasonCode":"NativeReason","additionalInfo":"PRIVATE-REPLY-112"},"customData":{"vendorId":"vendor","private":"PRIVATE-REPLY-112"}}])).await;
    let safe = accepted.await.unwrap().json::<Value>().await.unwrap()["result"].clone();
    assert_eq!(safe["local_authorization_201"]["status"], "Accepted");
    assert!(!safe.to_string().contains("PRIVATE-REPLY-112"));
    learn(
        &client,
        &fixture,
        &mut socket,
        "items-zero",
        "ItemsPerMessage",
        "0",
    )
    .await;
    let rejected = begin(
        &client,
        &fixture,
        command("known-item-limit", "SendLocalList", 1),
    )
    .await
    .unwrap();
    assert_eq!(rejected.status(), 400);
    let value = result(&client, &fixture, "known-item-limit").await;
    assert_eq!(value["lifecycle"]["stage"], "rejected");
    assert!(value["local_authorization_201"].is_null());
    no_call(&mut socket).await;
    socket.close(None).await.unwrap();
    let mut next = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut next).await;
    fixture.connected(&client, "station-a").await;
    let accepted = begin(
        &client,
        &fixture,
        command("new-socket-unknown-limits", "SendLocalList", 1),
    );
    assert_eq!(receive(&mut next).await[3], native(1));
    send(
        &mut next,
        json!([3,"new-socket-unknown-limits",{"status":"Accepted"}]),
    )
    .await;
    assert_eq!(accepted.await.unwrap().status(), 202);
    assert_private(&fixture, &safe);
}
