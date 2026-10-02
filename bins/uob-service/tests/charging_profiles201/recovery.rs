use super::{
    peer::{Peer, Socket},
    support::*,
};
use serde_json::{Value, json};

#[tokio::test]
async fn charger_install_survives_bridge_and_peer_restart_without_blind_resend_or_phase_snapshot_authority()
 {
    let fixture = Fixture::new(true);
    let (process, client, mut socket, mut peer) = start_peer(&fixture).await;
    baseline(&client, &fixture, &mut socket, &mut peer).await;
    peer.set_phase(1, true);
    exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        phase_query("phase-before-restart"),
    )
    .await;
    let mut payload = profile(118, "0");
    payload["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["numberPhases"] =
        json!(1);
    payload["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["phaseToUse"] =
        json!(3);
    let original = command(
        "uncertain-after-install",
        1,
        "SetChargingProfile",
        payload.clone(),
    );
    peer.controls.disconnect_after_apply = true;
    let uncertain = exchange(&client, &fixture, &mut socket, &mut peer, original.clone()).await;
    assert_eq!(uncertain["lifecycle"]["stage"], "transmission_uncertain");
    assert!(uncertain.get("charging_profile_201").is_none());
    assert_eq!(peer.profiles(), &[payload.clone()]);
    disconnected(&client, &fixture).await;
    drop(socket);
    drop(process);
    drop(peer);
    let mut peer = fixture.peer();
    assert_eq!(peer.profiles(), &[payload.clone()]);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    assert_eq!(
        result(&client, &fixture, "uncertain-after-install").await,
        uncertain
    );
    let mut socket = fixture.station201().await;
    fixture.connected(&client, "station-b").await;
    Peer::boot(&mut socket).await.unwrap();
    assert_replay_without_call(&client, &fixture, &mut socket, &original, &uncertain).await;
    denied(
        &client,
        &fixture,
        command("phase-not-restored", 1, "SetChargingProfile", payload),
    )
    .await;
    denied(
        &client,
        &fixture,
        command(
            "uncertain-owned-conflict",
            1,
            "SetChargingProfile",
            profile(119, "0"),
        ),
    )
    .await;
    no_call(&mut socket).await;
    assert_eq!(peer.counters.set, 0);
    let clear = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command(
            "recovery-clear",
            0,
            "ClearChargingProfile",
            json!({"chargingProfileId":118}),
        ),
    )
    .await;
    assert_eq!(clear["charging_profile_201"]["status"], "Accepted");
    assert!(peer.profiles().is_empty());
    let installed = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command("after-recovery", 1, "SetChargingProfile", profile(119, "0")),
    )
    .await;
    assert_eq!(installed["charging_profile_201"]["status"], "Accepted");
    assert_eq!(peer.profiles(), &[profile(119, "0")]);
}

#[tokio::test]
async fn canonical_only_mode_works_without_baseline_and_does_not_fabricate_full_native_evidence() {
    let fixture = Fixture::new(false);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station201().await;
    fixture.connected(&client, "station-b").await;
    let mut peer = fixture.peer();
    Peer::boot(&mut socket).await.unwrap();
    peer.transaction(&mut socket, 1, "canonical-native-118", false)
        .await
        .unwrap();
    let canonical = json!({"request_id":"canonical-only","resource":resource(1),"expires_at":"2099-01-01T00:00:00Z",
        "operation":{"kind":"set_charging_limit","parameters":{"value":"8.1","unit":"ampere","phases":1}}});
    let worker = client.clone();
    let url = fixture.url("/api/v1/commands");
    let request = canonical.clone();
    let pending = tokio::spawn(async move {
        worker
            .post(url)
            .bearer_auth(CONTROL)
            .json(&request)
            .send()
            .await
            .unwrap()
    });
    let call = peer.serve_one(&mut socket).await.unwrap();
    assert_eq!(call[2], "SetChargingProfile");
    assert_eq!(
        call[3]["chargingProfile"]["transactionId"],
        "canonical-native-118"
    );
    let accepted = completed(pending).await;
    assert_eq!(accepted["lifecycle"]["stage"], "protocol_response");
    assert_eq!(accepted["lifecycle"]["accepted"], true);
    assert!(accepted.get("charging_profile_201").is_none());
    assert_eq!(peer.profiles(), &[call[3].clone()]);
    let duplicate = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&canonical)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.json::<Value>().await.unwrap()["result"], accepted);
    no_call(&mut socket).await;
    let removed = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command(
            "clear-canonical-profile",
            0,
            "ClearChargingProfile",
            json!({"chargingProfileId":call[3]["chargingProfile"]["id"]}),
        ),
    )
    .await;
    assert_eq!(removed["charging_profile_201"]["status"], "Accepted");
    assert!(peer.profiles().is_empty());
}

#[tokio::test]
async fn full_native_mode_also_blocks_canonical_set_until_explicit_baseline() {
    let fixture = Fixture::new(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station201().await;
    fixture.connected(&client, "station-b").await;
    let mut peer = fixture.peer();
    Peer::boot(&mut socket).await.unwrap();
    peer.transaction(&mut socket, 1, "canonical-blocked-118", false)
        .await
        .unwrap();
    let body = json!({"request_id":"canonical-before-baseline","resource":resource(1),"expires_at":"2099-01-01T00:00:00Z",
        "operation":{"kind":"set_charging_limit","parameters":{"value":"8.1","unit":"ampere","phases":1}}});
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&body)
        .send()
        .await
        .unwrap();
    let denied: Value = response.json().await.unwrap();
    assert_eq!(denied["lifecycle"]["stage"], "rejected");
    assert_eq!(denied["lifecycle"]["error"]["code"], "policy_rejected");
    no_call(&mut socket).await;
    assert!(peer.profiles().is_empty());
    assert_eq!(peer.counters.set, 0);
}

async fn canonical_exchange(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    peer: &mut Peer,
    body: Value,
) -> (Value, Value) {
    let worker = client.clone();
    let url = fixture.url("/api/v1/commands");
    let pending = tokio::spawn(async move {
        worker
            .post(url)
            .bearer_auth(CONTROL)
            .json(&body)
            .send()
            .await
            .unwrap()
    });
    let call = peer.serve_one(socket).await.unwrap();
    (completed(pending).await, call)
}

#[tokio::test]
async fn canonical_control_cannot_replace_a_privileged_policy_that_shares_its_stable_transaction_id()
 {
    let fixture = Fixture::new(true);
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station201().await;
    fixture.connected(&client, "station-b").await;
    let mut peer = fixture.peer();
    Peer::boot(&mut socket).await.unwrap();
    baseline(&client, &fixture, &mut socket, &mut peer).await;
    peer.transaction(&mut socket, 1, "canonical-native-collision", false)
        .await
        .unwrap();
    let mut canonical = json!({"request_id":"canonical-initial","resource":resource(1),
        "expires_at":"2099-01-01T00:00:00Z","operation":{"kind":"set_charging_limit",
        "parameters":{"value":"8.1","unit":"ampere","phases":1}}});
    let (initial, first) =
        canonical_exchange(&client, &fixture, &mut socket, &mut peer, canonical.clone()).await;
    assert_eq!(initial["lifecycle"]["accepted"], true);
    assert!(initial.get("charging_profile_201").is_none());
    canonical["request_id"] = json!("canonical-repeat");
    canonical["operation"]["parameters"]["value"] = json!("9.2");
    let (repeated, next) =
        canonical_exchange(&client, &fixture, &mut socket, &mut peer, canonical.clone()).await;
    assert_eq!(repeated["lifecycle"]["accepted"], true);
    assert!(repeated.get("charging_profile_201").is_none());
    assert_eq!(
        next[3]["chargingProfile"]["id"],
        first[3]["chargingProfile"]["id"]
    );
    assert_eq!(peer.profiles(), &[next[3].clone()]);
    let stable_id = i32::try_from(first[3]["chargingProfile"]["id"].as_i64().unwrap()).unwrap();
    let protected = profile(stable_id, "0");
    let native = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command(
            "privileged-same-id-policy",
            1,
            "SetChargingProfile",
            protected.clone(),
        ),
    )
    .await;
    assert_eq!(native["charging_profile_201"]["status"], "Accepted");
    assert_eq!(peer.profiles(), std::slice::from_ref(&protected));
    canonical["request_id"] = json!("canonical-policy-collision");
    canonical["operation"]["parameters"]["value"] = json!("10.3");
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&canonical)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let rejected: Value = response.json().await.unwrap();
    assert_eq!(rejected["lifecycle"]["stage"], "rejected");
    assert_eq!(rejected["lifecycle"]["error"]["code"], "policy_rejected");
    assert!(rejected.get("charging_profile_201").is_none());
    no_call(&mut socket).await;
    assert_eq!(peer.profiles(), &[protected]);
    assert_eq!(
        result(&client, &fixture, "privileged-same-id-policy").await,
        native
    );
    drop(socket);
    disconnected(&client, &fixture).await;
    drop(process);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
