use super::support::*;
use serde_json::{Value, json};
use std::fs;

fn child(connector: bool) -> Value {
    let mut resource = json!({"bridge_id":"bridge-1","station_id":"station-a",
        "resource":{"kind":"evse","evse_id":"evse-one"},
        "native_protocol_reference":{"protocol":"ocpp201","evse_id":1}});
    if connector {
        resource["resource"]["connector_id"] = json!("one");
        resource["native_protocol_reference"]["connector_id"] = json!(1);
    }
    resource
}
fn scoped_entry(connector: bool) -> Value {
    let mut value = entry(if connector { 91 } else { 92 }, "Password");
    value["component"]["evse"] = json!({"id":1});
    if connector {
        value["component"]["evse"]["connectorId"] = json!(1);
    }
    value
}

#[tokio::test]
async fn exact_configured_evse_and_connector_writes_require_contained_selectors_and_resource_binding()
 {
    let fixture = fixture();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path).unwrap().replace(
        "[[charging.stations]]\nid='station-b'",
        "[[charging.stations.resources]]\nevse_id='evse-one'\nnative_evse_id=1\n[[charging.stations]]\nid='station-b'",
    );
    fs::write(path, config).unwrap();
    let mut data = provisioning();
    for connector in [false, true] {
        data["variables"]
            .as_array_mut()
            .unwrap()
            .push(json!({"resource":child(connector),
            "entry":scoped_entry(connector),"value":SECRET,"expires_at":"2099-01-01T00:00:00Z"}));
    }
    save_provisioning(&fixture, &data);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let mut peer = Peer::default();
    for connector in [false, true] {
        let id = if connector {
            "connector-write"
        } else {
            "evse-write"
        };
        let mut body = variables(id, vec![scoped_entry(connector)]);
        body["resource"] = child(connector);
        let request = begin(&client, &fixture, body);
        let call = receive(&mut station).await;
        let evse = if connector {
            json!({"id":1,"connectorId":1})
        } else {
            json!({"id":1})
        };
        assert_eq!(
            call,
            json!([2,id,"SetVariables",{"setVariableData":[{
            "component":{"name":"VendorCtrlr","evse":evse},"variable":{"name":"Password"},"attributeValue":SECRET}]}])
        );
        send(&mut station, json!([3, id, peer.set_variables(&call[3])])).await;
        assert_eq!(completed(request).await["lifecycle"]["accepted"], true);
    }
    let mut wrong_resource = variables("wrong-exact-resource", vec![scoped_entry(true)]);
    wrong_resource["resource"] = child(false);
    assert_not_admitted(&client, &fixture, wrong_resource).await;
    let mut uncontained = scoped_entry(true);
    uncontained["component"]["evse"]["connectorId"] = json!(2);
    let mut body = variables("uncontained-selector", vec![uncontained]);
    body["resource"] = child(true);
    assert_not_admitted(&client, &fixture, body).await;
    let mut wrong_roster = variables("unconfigured-evse", vec![scoped_entry(false)]);
    wrong_roster["resource"] = child(false);
    wrong_roster["resource"]["resource"]["evse_id"] = json!("evse-other");
    wrong_roster["resource"]["native_protocol_reference"]["evse_id"] = json!(2);
    assert_not_admitted(&client, &fixture, wrong_roster).await;
    no_call(&mut station).await;
}
