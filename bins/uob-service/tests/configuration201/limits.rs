use super::support::*;
use serde_json::json;

async fn limit(
    client: &reqwest::Client,
    fixture: &Fixture,
    station: &mut Socket,
    id: &str,
    bytes: usize,
) {
    let identity = json!({"component":{"name":"DeviceDataCtrlr"},"variable":{"name":"BytesPerMessage","instance":"SetVariables"}});
    let request = begin(
        client,
        fixture,
        command(id, "GetVariables", json!({"getVariableData":[identity]})),
    );
    assert_eq!(
        receive(station).await,
        json!([2,id,"GetVariables",{"getVariableData":[identity]}])
    );
    send(station,json!([3,id,{"getVariableResult":[{"component":{"name":"DeviceDataCtrlr"},"variable":{"name":"BytesPerMessage","instance":"SetVariables"},"attributeStatus":"Accepted","attributeValue":bytes.to_string()}]}])).await;
    completed(request).await;
}

#[tokio::test]
async fn native_item_limit_and_exact_escaped_frame_byte_limit_reject_without_splitting() {
    let fixture = fixture();
    let mut data = provisioning();
    data["variables"][8]["value"] = json!("\"\\\n🦀");
    save_provisioning(&fixture, &data);
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    learn_limits(&client, &fixture, &mut station, "1", "65536").await;
    let denied = known_not_sent(
        &client,
        &fixture,
        begin(
            &client,
            &fixture,
            variables("too-many", vec![entry(1, "Empty"), entry(9, "Password")]),
        ),
    )
    .await;
    assert_eq!(denied["lifecycle"]["stage"], "rejected");
    no_call(&mut station).await;
    // Serialize an independently authored expected native frame, never adapter output.
    let expected = json!([2,"byte-budget","SetVariables",{"setVariableData":[{
        "component":{"name":"VendorCtrlr"},"variable":{"name":"Password"},"attributeValue":"\"\\\n🦀"}]}]);
    let bytes = expected.to_string().len();
    // Use fresh request IDs for explicit evidence updates in the same socket generation.
    limit(&client, &fixture, &mut station, "tighten", bytes - 1).await;
    let denied = known_not_sent(
        &client,
        &fixture,
        begin(
            &client,
            &fixture,
            variables("byte-budget", vec![entry(9, "Password")]),
        ),
    )
    .await;
    assert_eq!(denied["lifecycle"]["stage"], "rejected");
    no_call(&mut station).await;
    limit(&client, &fixture, &mut station, "exact", bytes).await;
    // Same-length new ID avoids retrying the durably rejected command.
    let request = begin(
        &client,
        &fixture,
        variables("byte-exact!", vec![entry(9, "Password")]),
    );
    let call = receive(&mut station).await;
    let mut expected = expected;
    expected[1] = json!("byte-exact!");
    assert_eq!(call, expected);
    let mut peer = Peer::default();
    send(
        &mut station,
        json!([3, call[1], peer.set_variables(&call[3])]),
    )
    .await;
    assert_eq!(completed(request).await["lifecycle"]["accepted"], true);
}

#[tokio::test]
async fn malformed_native_variable_ack_is_uncertainty_not_fabricated_success() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    for (id, reply) in [
        ("missing", json!({"setVariableResult":[]})),
        (
            "mismatch",
            json!({"setVariableResult":[{"component":{"name":"Other"},"variable":{"name":"Password"},"attributeStatus":"Accepted"}]}),
        ),
        (
            "duplicate",
            json!({"setVariableResult":[
            {"component":{"name":"VendorCtrlr"},"variable":{"name":"Password"},"attributeStatus":"Accepted"},
            {"component":{"name":"VendorCtrlr"},"variable":{"name":"Password"},"attributeStatus":"Accepted"}]}),
        ),
    ] {
        let request = begin(&client, &fixture, variables(id, vec![entry(9, "Password")]));
        assert_eq!(receive(&mut station).await[1], id);
        send(&mut station, json!([3, id, reply])).await;
        let value = completed(request).await;
        assert_eq!(value["lifecycle"]["stage"], "transmission_uncertain");
        assert!(value["configuration_201"].is_null());
        assert_no_secrets(&fixture, &value);
    }
}
