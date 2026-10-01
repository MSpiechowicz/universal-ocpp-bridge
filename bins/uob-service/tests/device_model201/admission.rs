use super::support::*;
use serde_json::{Value, json};

#[tokio::test]
async fn default_off_unprivileged_and_opaque_queries_never_reach_native_socket() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    let mut disabled = fixture.station_protocol("station-b", "ocpp2.0.1").await;
    boot(&mut station).await;
    boot(&mut disabled).await;
    let query = command(
        "authority",
        "GetVariables",
        json!({"getVariableData":[{"component":{"name":"Ctrlr"},"variable":{"name":"Value"}}]}),
    );
    let denied = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&query)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    no_call(&mut station).await;
    let mut off = query.clone();
    off["request_id"] = json!("disabled");
    off["resource"]["station_id"] = json!("station-b");
    let denied = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&off)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 422);
    no_call(&mut disabled).await;
    for (id, payload) in [
        (
            "opaque",
            json!({"requestId":1,"reportBase":"FullInventory","customData":{"vendorId":"SECRET-REQUEST"}}),
        ),
        (
            "native-overflow",
            json!({"requestId":2_147_483_648_u64,"reportBase":"FullInventory"}),
        ),
    ] {
        let body = command(id, "GetBaseReport", payload);
        let denied = client
            .post(fixture.url("/api/v1/commands"))
            .bearer_auth(PRIVILEGED)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 400);
        let response: Value = denied.json().await.unwrap();
        assert!(!response.to_string().contains("SECRET-REQUEST"));
        no_call(&mut station).await;
    }
}
