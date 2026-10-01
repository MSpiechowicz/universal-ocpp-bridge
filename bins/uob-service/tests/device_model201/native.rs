use super::support::*;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn accepted_report_stays_pending_while_heartbeat_progresses_then_completes_without_secrets() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let body = command(
        "inventory",
        "GetBaseReport",
        json!({"requestId":-9,"reportBase":"FullInventory"}),
    );
    let request = begin(&client, &fixture, body.clone());
    let call = receive(&mut station).await;
    assert_eq!(
        call,
        json!([2,"inventory","GetBaseReport",{"requestId":-9,"reportBase":"FullInventory"}])
    );
    assert_eq!(
        result(&client, &fixture, "inventory").await["lifecycle"]["stage"],
        "dispatched"
    );
    send(&mut station, json!([3,"inventory",{"status":"Accepted"}])).await;
    let pending = completed(request).await;
    assert_eq!(pending["device_model_201"]["report"]["state"], "pending");
    tokio::time::timeout(
        Duration::from_secs(2),
        station_call(&mut station, "heartbeat", "Heartbeat", json!({})),
    )
    .await
    .expect("heartbeat during collection");
    send(&mut station, json!([2,"fragment","NotifyReport",{"requestId":-9,"generatedAt":"2026-09-01T00:00:00Z","seqNo":0,"reportData":[{"component":{"name":"VendorCtrlr","customData":{"vendorId":"SECRET-VENDOR"}},"variable":{"name":"Password"},"variableAttribute":[{"value":"SECRET-PASSWORD","mutability":"WriteOnly"}],"variableCharacteristics":{"dataType":"string","supportsMonitoring":false,"valuesList":"SECRET-LIST"}}]}])).await;
    assert_eq!(receive(&mut station).await, json!([3, "fragment", {}]));
    let full = terminal(&client, &fixture, "inventory").await;
    assert_eq!(full["device_model_201"]["report"]["state"], "complete");
    assert_eq!(full["device_model_201"]["report"]["progress"]["items"], 1);
    let value = &full["device_model_201"]["report"]["items"][0]["attributes"][0]["value"];
    assert_eq!(value["present"], true);
    assert_eq!(value["redacted"], true);
    assert!(value["value"].is_null());
    for marker in ["SECRET-VENDOR", "SECRET-PASSWORD", "SECRET-LIST"] {
        assert!(!full.to_string().contains(marker));
    }
    assert_eq!(completed(begin(&client, &fixture, body)).await, full);
    no_call(&mut station).await;
    let reused = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&command(
            "reused-native-id",
            "GetReport",
            json!({"requestId":-9}),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(reused.status(), 400);
    no_call(&mut station).await;
}

#[tokio::test]
async fn get_variables_returns_native_unknown_and_explicit_empty_without_secret_disclosure() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    for (id, status, value) in [
        ("unknown", "UnknownVariable", None),
        ("empty", "Accepted", Some("")),
        ("redacted", "Accepted", Some("SECRET-VALUE")),
    ] {
        let body = command(
            id,
            "GetVariables",
            json!({"getVariableData":[{"component":{"name":"SecurityCtrlr"},"variable":{"name":"Password"}}]}),
        );
        let request = begin(&client, &fixture, body);
        let call = receive(&mut station).await;
        assert_eq!(call[2], "GetVariables");
        let mut entry = json!({"attributeStatus":status,"component":{"name":"SecurityCtrlr"},"variable":{"name":"Password"}});
        if let Some(value) = value {
            entry["attributeValue"] = json!(value);
        }
        send(
            &mut station,
            json!([3,call[1],{"getVariableResult":[entry]}]),
        )
        .await;
        let result = completed(request).await;
        let evidence = &result["device_model_201"]["variables"][0];
        assert_eq!(evidence["status"], status);
        assert_eq!(evidence["value"]["present"], value.is_some());
        assert_eq!(evidence["value"]["empty"], value == Some(""));
        assert!(!result.to_string().contains("SECRET-VALUE"));
    }
}
