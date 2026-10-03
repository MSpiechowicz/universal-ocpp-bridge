use super::support::*;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn native_empty_unicode_all_statuses_and_independent_readonly_range_policy() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let mut peer = Peer::default();
    for (index, name, status, value) in [
        (1, "Empty", "Accepted", String::new()),
        (2, "Unicode", "Accepted", "🦀".repeat(1000)),
        (3, "ReadOnly", "Rejected", SECRET.to_owned()),
        (4, "Range", "Rejected", "101".to_owned()),
        (5, "UnknownComponent", "UnknownComponent", SECRET.to_owned()),
        (6, "UnknownVariable", "UnknownVariable", SECRET.to_owned()),
        (
            7,
            "Unsupported",
            "NotSupportedAttributeType",
            SECRET.to_owned(),
        ),
        (8, "Reboot", "RebootRequired", SECRET.to_owned()),
    ] {
        let id = format!("write-{index}");
        let body = variables(&id, vec![entry(index, name)]);
        let request = begin(&client, &fixture, body.clone());
        let call = receive(&mut station).await;
        assert_eq!(
            call,
            json!([2,id,"SetVariables",{"setVariableData":[{
            "component":{"name":"VendorCtrlr"},"variable":{"name":name},"attributeValue":value}]}])
        );
        assert_eq!(
            result(&client, &fixture, &id).await["lifecycle"]["stage"],
            "dispatched"
        );
        let heartbeat = tokio::time::timeout(
            Duration::from_secs(2),
            station_call(&mut station, &format!("hb-{index}"), "Heartbeat", json!({})),
        )
        .await
        .unwrap();
        assert!(heartbeat["currentTime"].is_string());
        send(&mut station, json!([3, id, peer.set_variables(&call[3])])).await;
        let value = completed(request).await;
        assert_eq!(value["configuration_201"]["variables"][0]["status"], status);
        assert_eq!(
            value["lifecycle"]["accepted"],
            status == "Accepted" || status == "RebootRequired"
        );
        assert_eq!(result(&client, &fixture, &id).await, value);
        assert_no_secrets(&fixture, &value);
        assert_eq!(completed(begin(&client, &fixture, body)).await, value);
        no_call(&mut station).await;
    }
    assert_eq!(peer.values["Empty"], "");
    assert_eq!(peer.values["Unicode"].chars().count(), 1000);
    assert!(!peer.values.contains_key("ReadOnly"));
    assert!(!peer.values.contains_key("Range"));
}

#[tokio::test]
async fn explicit_native_limits_enable_multiitem_reordered_mixed_results() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let body = variables(
        "unknown-limits",
        vec![entry(1, "Empty"), entry(3, "ReadOnly")],
    );
    let denied = known_not_sent(&client, &fixture, begin(&client, &fixture, body)).await;
    assert_ne!(denied["lifecycle"]["stage"], "protocol_response");
    no_call(&mut station).await;
    learn_limits(&client, &fixture, &mut station, "3", "65536").await;
    let request = begin(
        &client,
        &fixture,
        variables(
            "mixed",
            vec![entry(1, "Empty"), entry(3, "ReadOnly"), entry(8, "Reboot")],
        ),
    );
    let call = receive(&mut station).await;
    assert_eq!(
        call,
        json!([2,"mixed","SetVariables",{"setVariableData":[
        {"component":{"name":"VendorCtrlr"},"variable":{"name":"Empty"},"attributeValue":""},
        {"component":{"name":"VendorCtrlr"},"variable":{"name":"ReadOnly"},"attributeValue":SECRET},
        {"component":{"name":"VendorCtrlr"},"variable":{"name":"Reboot"},"attributeValue":SECRET}]}])
    );
    let mut peer = Peer::default();
    send(
        &mut station,
        json!([3, "mixed", peer.set_variables(&call[3])]),
    )
    .await;
    let value = completed(request).await;
    assert_eq!(value["lifecycle"]["accepted"], false);
    let items = value["configuration_201"]["variables"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    for (name, status) in [
        ("Empty", "Accepted"),
        ("ReadOnly", "Rejected"),
        ("Reboot", "RebootRequired"),
    ] {
        assert!(
            items
                .iter()
                .any(|item| item["variable"]["name"] == name && item["status"] == status)
        );
    }
    assert_no_secrets(&fixture, &value);
    let request = begin(
        &client,
        &fixture,
        variables("all-positive", vec![entry(1, "Empty"), entry(8, "Reboot")]),
    );
    let call = receive(&mut station).await;
    send(
        &mut station,
        json!([3, call[1], peer.set_variables(&call[3])]),
    )
    .await;
    assert_eq!(completed(request).await["lifecycle"]["accepted"], true);
}

#[tokio::test]
async fn network_statuses_preserve_staging_until_controlled_peer_reboot() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let old = profile("wss://old-active.invalid/ocpp");
    let mut peer = Peer {
        active: Some(old.clone()),
        ..Peer::default()
    };
    for (slot, status) in [(0, "Accepted"), (1, "Rejected"), (2, "Failed")] {
        let id = format!("network-{slot}");
        let request = begin(&client, &fixture, network(&id, slot));
        let call = receive(&mut station).await;
        assert_eq!(
            call,
            json!([2,id,"SetNetworkProfile",{"configurationSlot":slot,"connectionData":profile(&format!("wss://csms-{slot}.invalid/ocpp"))}])
        );
        send(&mut station, json!([3, id, peer.set_network(&call[3])])).await;
        let value = completed(request).await;
        assert_eq!(
            value["configuration_201"],
            json!({"action":"SetNetworkProfile","configuration_slot":slot,"status":status,"staged":slot == 0})
        );
        assert_eq!(value["lifecycle"]["accepted"], slot == 0);
        assert!(
            value
                .get("observed_effects")
                .is_none_or(|effects| effects == &json!([]))
        );
        assert_eq!(peer.active, Some(old.clone()));
        assert_no_secrets(&fixture, &value);
    }
    assert!(!peer.staged.contains_key(&1));
    assert!(!peer.staged.contains_key(&2));
    no_call(&mut station).await;
    peer.reboot(0);
    assert_eq!(peer.active, Some(profile("wss://csms-0.invalid/ocpp")));
    assert_eq!(
        result(&client, &fixture, "network-0").await["configuration_201"]["staged"],
        true
    );
}
