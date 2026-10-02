use super::{
    peer::{Peer, Reply},
    support::*,
};
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn explicit_baseline_native_replacement_denial_selective_clear_and_peer_reopen() {
    let fixture = Fixture::new(true);
    let (_process, client, mut socket, mut peer) = start_peer(&fixture).await;
    let body = command(
        "baseline-blocked",
        1,
        "SetChargingProfile",
        profile(-118, "0"),
    );
    denied(&client, &fixture, body).await;
    no_call(&mut socket).await;
    baseline(&client, &fixture, &mut socket, &mut peer).await;
    let body = command(
        "native-install",
        1,
        "SetChargingProfile",
        profile(-118, "900719925474099.1"),
    );
    let result = exchange(&client, &fixture, &mut socket, &mut peer, body.clone()).await;
    assert_eq!(result["charging_profile_201"]["status"], "Accepted");
    assert_eq!(
        result["charging_profile_201"]["request"]["charging_profile"]["charging_schedule"][0]["charging_schedule_period"]
            [0]["limit"],
        "900719925474099.1"
    );
    assert_eq!(
        peer.profiles(),
        &[body["operation"]["parameters"]["payload"].clone()]
    );
    assert_replay_without_call(&client, &fixture, &mut socket, &body, &result).await;
    peer.controls.reject_next = true;
    let denial = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command(
            "denied-replacement",
            1,
            "SetChargingProfile",
            profile(-118, "0"),
        ),
    )
    .await;
    assert_eq!(denial["charging_profile_201"]["status"], "Rejected");
    assert_eq!(
        peer.profiles()[0],
        body["operation"]["parameters"]["payload"]
    );
    let replaced = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command(
            "accepted-replacement",
            1,
            "SetChargingProfile",
            profile(-118, "0"),
        ),
    )
    .await;
    assert_eq!(replaced["charging_profile_201"]["status"], "Accepted");
    assert_eq!(peer.profiles(), &[profile(-118, "0")]);
    let no_match = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command(
            "wrong-selector",
            0,
            "ClearChargingProfile",
            json!({"chargingProfileCriteria":{"evseId":2,"stackLevel":2}}),
        ),
    )
    .await;
    assert_eq!(no_match["charging_profile_201"]["status"], "Unknown");
    assert_eq!(peer.profiles(), &[profile(-118, "0")]);
    drop(peer);
    let mut peer = fixture.peer();
    assert_eq!(
        peer.profiles(),
        &[profile(-118, "0")],
        "the charger's private state survives independent reopen"
    );
    let removed = exchange(&client,&fixture,&mut socket,&mut peer,command("selective-clear",1,"ClearChargingProfile",
        json!({"chargingProfileCriteria":{"evseId":1,"chargingProfilePurpose":"TxDefaultProfile","stackLevel":2}}))).await;
    assert_eq!(removed["charging_profile_201"]["status"], "Accepted");
    assert!(peer.profiles().is_empty());
    assert_eq!(
        peer.counters.set, 0,
        "reopened peer counters are independent from persisted bridge results"
    );
}

#[tokio::test]
async fn native_transaction_and_phase_evidence_are_current_and_heartbeat_remains_active_during_delay()
 {
    let fixture = Fixture::new(true);
    let (process, client, mut socket, mut peer) = start_peer(&fixture).await;
    baseline(&client, &fixture, &mut socket, &mut peer).await;
    let native = establish_phase_transaction(&client, &fixture, &mut socket, &mut peer).await;
    let body = command("phase-and-tx", 1, "SetChargingProfile", native.clone());
    let pending = begin(&client, &fixture, body);
    let call = receive(&mut socket).await;
    let Reply::Result(reply) = peer.apply("SetChargingProfile", &call[3]).unwrap() else {
        panic!("native reply")
    };
    assert_eq!(reply["status"], "Accepted");
    let heartbeat = station_call(&mut socket, "during-profile-delay", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
    // A second mutation is explicitly busy while the first acknowledgement is withheld.
    let busy = denied(
        &client,
        &fixture,
        command(
            "busy-clear",
            0,
            "ClearChargingProfile",
            json!({"chargingProfileId":118}),
        ),
    )
    .await;
    assert_eq!(busy["lifecycle"]["error"]["code"], "policy_rejected");
    assert_eq!(peer.profiles(), std::slice::from_ref(&native));
    no_call(&mut socket).await;
    // Actual Ended commits while the full native Set acknowledgement is still withheld.
    peer.transaction(&mut socket, 1, "Native-Case118/EVSE1/Generation-0001", true)
        .await
        .unwrap();
    assert!(peer.profiles().is_empty());
    send(&mut socket, json!([3, call[1], reply])).await;
    let accepted = completed(pending).await;
    assert_eq!(
        accepted["charging_profile_201"]["request"]["charging_profile"]["transaction_id"],
        "Native-Case118/EVSE1/Generation-0001"
    );
    assert!(peer.profiles().is_empty());
    assert_eq!(result(&client, &fixture, "phase-and-tx").await, accepted);
    assert_phase_revocation_and_transaction_retirement(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        native,
        &accepted,
    )
    .await;
    drop(socket);
    disconnected(&client, &fixture).await;
    drop(process);
    tokio::time::sleep(Duration::from_millis(50)).await;
}

async fn establish_phase_transaction(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut super::peer::Socket,
    peer: &mut Peer,
) -> Value {
    peer.transaction(socket, 1, "Native-Case118/EVSE1/Generation-0001", false)
        .await
        .unwrap();
    let mut native = profile(118, "0");
    native["chargingProfile"]["chargingProfilePurpose"] = json!("TxProfile");
    native["chargingProfile"]["transactionId"] = json!("Native-Case118/EVSE1/Generation-0001");
    native["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["numberPhases"] =
        json!(1);
    native["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["phaseToUse"] =
        json!(2);
    denied(
        client,
        fixture,
        command("no-phase-proof", 1, "SetChargingProfile", native.clone()),
    )
    .await;
    no_call(socket).await;
    peer.set_phase(1, true);
    exchange(client, fixture, socket, peer, phase_query("phase-positive")).await;
    native
}

async fn assert_phase_revocation_and_transaction_retirement(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut super::peer::Socket,
    peer: &mut Peer,
    mut native: Value,
    accepted: &Value,
) {
    peer.set_phase(1, false);
    exchange(client, fixture, socket, peer, phase_query("phase-revoked")).await;
    let mut phase_default = native.clone();
    phase_default["chargingProfile"]["id"] = json!(119);
    phase_default["chargingProfile"]["chargingProfilePurpose"] = json!("TxDefaultProfile");
    phase_default["chargingProfile"]
        .as_object_mut()
        .unwrap()
        .remove("transactionId");
    denied(
        client,
        fixture,
        command("false-phase-proof", 1, "SetChargingProfile", phase_default),
    )
    .await;
    no_call(socket).await;
    native["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]
        .as_object_mut()
        .unwrap()
        .remove("phaseToUse");
    denied(
        client,
        fixture,
        command("ended-tx", 1, "SetChargingProfile", native),
    )
    .await;
    no_call(socket).await;
    assert!(peer.profiles().is_empty());
    peer.transaction(socket, 1, "Next-Native-Case-119", false)
        .await
        .unwrap();
    let mut next = profile(119, "0");
    next["chargingProfile"]["chargingProfilePurpose"] = json!("TxProfile");
    next["chargingProfile"]["transactionId"] = json!("Next-Native-Case-119");
    let after_end = exchange(
        client,
        fixture,
        socket,
        peer,
        command(
            "new-tx-after-late-ack",
            1,
            "SetChargingProfile",
            next.clone(),
        ),
    )
    .await;
    assert_eq!(after_end["charging_profile_201"]["status"], "Accepted");
    assert_eq!(peer.profiles(), &[next]);
    assert_eq!(&result(client, fixture, "phase-and-tx").await, accepted);
}

#[tokio::test]
async fn rejected_and_duplicate_transaction_commits_release_profile_dispatch_and_preserve_heartbeat()
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
    let timestamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let mut event = json!({"eventType":"Ended","timestamp":timestamp,"triggerReason":"StopAuthorized",
        "seqNo":1,"evse":{"id":1,"connectorId":1},
        "transactionInfo":{"transactionId":"missing-start","chargingState":"Idle"}});
    send(
        &mut socket,
        json!([2, "failed-commit", "TransactionEvent", event]),
    )
    .await;
    let denied = receive(&mut socket).await;
    assert_eq!(denied[0], 4);
    assert_eq!(denied[1], "failed-commit");
    assert_eq!(denied[2], "ProtocolError");
    let heartbeat = station_call(&mut socket, "after-failed-commit", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
    let accepted = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command(
            "after-failed-commit",
            1,
            "SetChargingProfile",
            profile(118, "0"),
        ),
    )
    .await;
    assert_eq!(accepted["charging_profile_201"]["status"], "Accepted");

    event["eventType"] = json!("Started");
    event["triggerReason"] = json!("CablePluggedIn");
    event["seqNo"] = json!(0);
    event["transactionInfo"]["chargingState"] = json!("Charging");
    station_call(
        &mut socket,
        "fresh-transaction",
        "TransactionEvent",
        event.clone(),
    )
    .await;
    station_call(
        &mut socket,
        "exact-transaction-duplicate",
        "TransactionEvent",
        event,
    )
    .await;
    let accepted = exchange(
        &client,
        &fixture,
        &mut socket,
        &mut peer,
        command(
            "after-duplicate",
            1,
            "SetChargingProfile",
            profile(118, "8.1"),
        ),
    )
    .await;
    assert_eq!(accepted["charging_profile_201"]["status"], "Accepted");
    assert_eq!(peer.profiles(), &[profile(118, "8.1")]);
    drop(socket);
    disconnected(&client, &fixture).await;
    drop(process);
    tokio::time::sleep(Duration::from_millis(50)).await;
}
