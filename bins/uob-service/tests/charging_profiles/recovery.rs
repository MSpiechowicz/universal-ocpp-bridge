use super::{peer::Peer, support::*};
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn delayed_profile_keeps_heartbeat_progress_and_terminal_duplicate_does_not_resend() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    let body = command(
        "delayed-profile",
        1,
        "SetChargingProfile",
        profile(1, -117, "0", "A"),
    );
    let pending = begin(&client, &fixture, body.clone());
    let call = receive(&mut station).await;
    assert_eq!(call[2], "SetChargingProfile");
    let heartbeat = tokio::time::timeout(
        Duration::from_secs(2),
        station_call(&mut station, "progress", "Heartbeat", json!({})),
    )
    .await
    .expect("heartbeat progresses before profile reply");
    assert!(heartbeat["currentTime"].as_str().is_some());
    assert!(!pending.is_finished());
    let dispatched = result(&client, &fixture, "delayed-profile").await;
    assert_eq!(dispatched["lifecycle"]["stage"], "dispatched");
    assert_eq!(
        completed(begin(&client, &fixture, body.clone())).await,
        dispatched
    );
    no_call(&mut station).await;
    send(&mut station, json!([3,call[1],{"status":"Accepted"}])).await;
    let complete = completed(pending).await;
    assert_eq!(complete["charging_profile_16"]["status"], "Accepted");
    assert_eq!(
        complete["charging_profile_16"]["request"]["cs_charging_profiles"]["charging_schedule"]["charging_schedule_period"]
            [0]["limit"],
        "0"
    );
    assert_eq!(completed(begin(&client, &fixture, body)).await, complete);
    no_call(&mut station).await;
}

#[tokio::test]
async fn disconnect_after_peer_apply_and_restart_keep_uncertainty_without_replay() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    let acknowledged = exchange(
        &client,
        &fixture,
        &mut station,
        command(
            "terminal-before-loss",
            0,
            "ClearChargingProfile",
            json!({"id":116}),
        ),
        "Unknown",
    )
    .await;
    let body = command(
        "applied-without-reply",
        1,
        "SetChargingProfile",
        profile(1, -117, "0", "W"),
    );
    let pending = begin(&client, &fixture, body.clone());
    let call = receive(&mut station).await;
    let mut peer = Peer::open(fixture.root.join("durable-peer.json"));
    assert_eq!(peer.apply(call[2].as_str().unwrap(), &call[3]), "Accepted");
    // The charger applies and persists, but no correlated reply reaches the bridge.
    drop(station);
    let uncertain = completed(pending).await;
    assert_uncertain(&uncertain);
    disconnected(&client, &fixture).await;
    let mut station = fixture.station("station-a").await;
    no_call(&mut station).await;
    boot(&mut station).await;
    assert_eq!(
        completed(begin(&client, &fixture, body.clone())).await,
        uncertain
    );
    no_call(&mut station).await;
    send(&mut station, json!([3,call[1],{"status":"Accepted"}])).await;
    station_call(&mut station, "stale-reply-barrier", "Heartbeat", json!({})).await;
    assert_eq!(
        result(&client, &fixture, "applied-without-reply").await,
        uncertain
    );
    drop(station);
    drop(child);
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let mut peer = Peer::open(fixture.root.join("durable-peer.json"));
    assert_eq!(
        peer.profiles()[0]["csChargingProfiles"]["chargingProfileId"],
        -117
    );
    assert_eq!(
        result(&client, &fixture, "terminal-before-loss").await,
        acknowledged
    );
    assert_eq!(
        result(&client, &fixture, "applied-without-reply").await,
        uncertain
    );
    let mut station = fixture.station("station-a").await;
    no_call(&mut station).await;
    boot(&mut station).await;
    assert_eq!(
        completed(begin(&client, &fixture, body.clone())).await,
        uncertain
    );
    no_call(&mut station).await;
    let mut conflict = body;
    conflict["operation"]["parameters"]["payload"]["csChargingProfiles"]["stackLevel"] = json!(3);
    assert_eq!(
        begin(&client, &fixture, conflict).await.unwrap().status(),
        409
    );
    no_call(&mut station).await;
    let clear = begin(
        &client,
        &fixture,
        command(
            "explicit-clear-after-restart",
            0,
            "ClearChargingProfile",
            json!({"id":-117,"connectorId":999}),
        ),
    );
    peer.serve_one(&mut station).await;
    let clear = completed(clear).await;
    assert_eq!(clear["charging_profile_16"]["status"], "Accepted");
    assert_eq!(clear["charging_profile_16"]["request"]["id"], -117);
    assert!(peer.profiles().is_empty());
    assert_eq!(
        result(&client, &fixture, "applied-without-reply").await,
        uncertain
    );
}

#[tokio::test]
async fn malformed_native_reply_callerror_and_timeout_do_not_fabricate_profile_reply() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    for (index, reply) in [
        json!({}),
        json!({"status":"Rejected"}),
        json!({"status":"Accepted","clearedIds":[117]}),
    ]
    .into_iter()
    .enumerate()
    {
        let pending = begin(
            &client,
            &fixture,
            command(
                &format!("malformed-{index}"),
                0,
                "ClearChargingProfile",
                json!({"id":117}),
            ),
        );
        let call = receive(&mut station).await;
        send(&mut station, json!([3, call[1], reply])).await;
        assert_uncertain(&completed(pending).await);
    }
    let pending = begin(
        &client,
        &fixture,
        command("native-error", 0, "ClearChargingProfile", json!({})),
    );
    let call = receive(&mut station).await;
    send(
        &mut station,
        json!([4, call[1], "NotSupported", "private charger secret", {}]),
    )
    .await;
    let error = completed(pending).await;
    assert_eq!(error["lifecycle"]["accepted"], false);
    assert!(error.get("charging_profile_16").is_none());
    assert!(!error.to_string().contains("private charger secret"));
    let body = command(
        "native-timeout",
        1,
        "SetChargingProfile",
        profile(1, 117, "0", "A"),
    );
    let pending = begin(&client, &fixture, body.clone());
    let call = receive(&mut station).await;
    let uncertain = tokio::time::timeout(Duration::from_secs(40), completed(pending))
        .await
        .expect("native response deadline terminates profile");
    assert_uncertain(&uncertain);
    send(&mut station, json!([3,call[1],{"status":"Accepted"}])).await;
    station_call(&mut station, "late-barrier", "Heartbeat", json!({})).await;
    assert_eq!(result(&client, &fixture, "native-timeout").await, uncertain);
    assert_eq!(completed(begin(&client, &fixture, body)).await, uncertain);
    no_call(&mut station).await;
}
fn assert_uncertain(value: &Value) {
    assert_eq!(value["lifecycle"]["stage"], "transmission_uncertain");
    assert!(value.get("charging_profile_16").is_none());
    assert!(value.get("observed_effects").is_none());
}
