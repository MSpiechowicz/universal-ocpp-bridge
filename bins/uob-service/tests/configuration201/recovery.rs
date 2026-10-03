use super::support::*;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn applied_network_disconnect_before_ack_is_uncertain_and_never_replayed_after_restart() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let body = network("applied-disconnect", 0);
    let request = begin(&client, &fixture, body.clone());
    let call = receive(&mut station).await;
    assert_eq!(
        call,
        json!([2,"applied-disconnect","SetNetworkProfile",{"configurationSlot":0,"connectionData":profile("wss://csms-0.invalid/ocpp")}])
    );
    let mut peer = Peer::default();
    assert_eq!(peer.set_network(&call[3])["status"], "Accepted");
    assert!(peer.active.is_none());
    assert_eq!(
        result(&client, &fixture, "applied-disconnect").await["lifecycle"]["stage"],
        "dispatched"
    );
    station.close(None).await.unwrap();
    let value = completed(request).await;
    assert_eq!(value["lifecycle"]["stage"], "transmission_uncertain");
    assert!(value["configuration_201"].is_null());
    drop(station);
    drop(child);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    assert_eq!(result(&client, &fixture, "applied-disconnect").await, value);
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    no_call(&mut station).await;
    assert_eq!(completed(begin(&client, &fixture, body)).await, value);
    no_call(&mut station).await;
    assert!(peer.active.is_none());
    assert_eq!(peer.staged[&0], profile("wss://csms-0.invalid/ocpp"));
    assert_no_secrets(&fixture, &value);
}

#[tokio::test]
async fn bridge_crash_while_ack_pending_recovers_uncertainty_without_replay() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let body = variables("crash-variable", vec![entry(9, "Password")]);
    let request = begin(&client, &fixture, body.clone());
    let call = receive(&mut station).await;
    let mut peer = Peer::default();
    peer.set_variables(&call[3]);
    assert_eq!(peer.values["Password"], SECRET);
    assert_eq!(
        result(&client, &fixture, "crash-variable").await["lifecycle"]["stage"],
        "dispatched"
    );
    drop(child);
    drop(station);
    request.abort();
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    let recovered = result(&client, &fixture, "crash-variable").await;
    assert_eq!(recovered["lifecycle"]["stage"], "transmission_uncertain");
    assert!(recovered["configuration_201"].is_null());
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    no_call(&mut station).await;
    assert_eq!(completed(begin(&client, &fixture, body)).await, recovered);
    no_call(&mut station).await;
    assert_no_secrets(&fixture, &recovered);
}

#[tokio::test]
async fn safe_startup_replacement_revokes_old_capability_and_uses_new_immutable_content() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let request = begin(
        &client,
        &fixture,
        variables("before-replace", vec![entry(9, "Password")]),
    );
    let call = receive(&mut station).await;
    assert_eq!(call[3]["setVariableData"][0]["attributeValue"], SECRET);
    let mut peer = Peer::default();
    send(
        &mut station,
        json!([3, call[1], peer.set_variables(&call[3])]),
    )
    .await;
    let historical = completed(request).await;
    assert_eq!(historical["lifecycle"]["accepted"], true);
    drop(child);
    drop(station);
    let mut data = provisioning();
    data["variables"][8]["entry"]["valueReference"] = json!(reference(90));
    data["variables"][8]["value"] = json!("replacement-private");
    // Operator replacement is an owner-only atomic rename while the daemon is stopped.
    save_provisioning(&fixture, &data);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    assert_not_admitted(
        &client,
        &fixture,
        variables("old-capability", vec![entry(9, "Password")]),
    )
    .await;
    no_call(&mut station).await;
    assert_eq!(
        completed(begin(
            &client,
            &fixture,
            variables("before-replace", vec![entry(9, "Password")])
        ))
        .await,
        historical
    );
    assert_eq!(
        result(&client, &fixture, "before-replace").await,
        historical
    );
    let wrong_grant = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&variables("before-replace", vec![entry(90, "Password")]))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_grant.status(), 403);
    let mut wrong_resource = variables("before-replace", vec![entry(9, "Password")]);
    wrong_resource["resource"]["station_id"] = json!("station-b");
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&wrong_resource)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    assert_eq!(
        result(&client, &fixture, "before-replace").await,
        historical
    );
    no_call(&mut station).await;
    let request = begin(
        &client,
        &fixture,
        variables("new-capability", vec![entry(90, "Password")]),
    );
    let call = receive(&mut station).await;
    assert_eq!(
        call,
        json!([2,"new-capability","SetVariables",{"setVariableData":[{"component":{"name":"VendorCtrlr"},"variable":{"name":"Password"},"attributeValue":"replacement-private"}]}])
    );
    send(
        &mut station,
        json!([3, call[1], peer.set_variables(&call[3])]),
    )
    .await;
    let value = completed(request).await;
    assert_eq!(value["lifecycle"]["accepted"], true);
    assert!(!value.to_string().contains("replacement-private"));
    assert_eq!(peer.values["Password"], "replacement-private");
}

#[tokio::test]
async fn expiry_is_rechecked_after_startup_and_reconnect_drops_learned_limits() {
    let fixture = fixture();
    let expires = time::OffsetDateTime::now_utc() + time::Duration::seconds(8);
    let mut data = provisioning();
    data["variables"][8]["expires_at"] = json!(
        expires
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    );
    save_provisioning(&fixture, &data);
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let request = begin(
        &client,
        &fixture,
        variables("before-live-expiry", vec![entry(9, "Password")]),
    );
    let call = receive(&mut station).await;
    assert_eq!(call[3]["setVariableData"][0]["attributeValue"], SECRET);
    let mut peer = Peer::default();
    send(
        &mut station,
        json!([3, call[1], peer.set_variables(&call[3])]),
    )
    .await;
    assert_eq!(completed(request).await["lifecycle"]["accepted"], true);
    learn_limits(&client, &fixture, &mut station, "4", "65536").await;
    let delay = expires - time::OffsetDateTime::now_utc();
    if delay.is_positive() {
        tokio::time::sleep(Duration::try_from(delay).unwrap()).await;
    }
    assert_not_admitted(
        &client,
        &fixture,
        variables("expired-live", vec![entry(9, "Password")]),
    )
    .await;
    no_call(&mut station).await;
    station.close(None).await.unwrap();
    drop(station);
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let unknown = known_not_sent(
        &client,
        &fixture,
        begin(
            &client,
            &fixture,
            variables(
                "new-generation",
                vec![entry(1, "Empty"), entry(8, "Reboot")],
            ),
        ),
    )
    .await;
    assert_eq!(unknown["lifecycle"]["stage"], "rejected");
    no_call(&mut station).await;
}
