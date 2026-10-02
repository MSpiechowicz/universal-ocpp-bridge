use super::{peer::Peer, support::*};
use serde_json::{Value, json};

#[tokio::test]
async fn live_native_a_w_zero_and_statuses_retain_typed_requests_without_effect_claims() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    let tx = start_transaction(&mut station).await;
    let mut peer = Peer::open(fixture.root.join("peer-state.json"));
    let mut results =
        verify_exact_tx_profiles(&client, &fixture, &mut station, &mut peer, tx).await;
    verify_peer_replacements_and_clears(&client, &fixture, &mut station, &mut peer).await;
    for (index, status) in ["Rejected", "NotSupported"].into_iter().enumerate() {
        let value = exchange(
            &client,
            &fixture,
            &mut station,
            command(
                &format!("set-status-{index}"),
                0,
                "SetChargingProfile",
                profile(0, 120, "0", "W"),
            ),
            status,
        )
        .await;
        assert_eq!(value["lifecycle"]["accepted"], false);
        assert_eq!(value["lifecycle"]["error"]["code"], "protocol_rejected");
        results.push((format!("set-status-{index}"), value));
    }
    drop(station);
    drop(child);
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let restored = Peer::open(fixture.root.join("peer-state.json"));
    assert_eq!(restored.profiles(), peer.profiles());
    for (id, value) in results {
        assert_eq!(result(&client, &fixture, &id).await, value);
    }
}

async fn verify_exact_tx_profiles(
    client: &reqwest::Client,
    fixture: &Fixture,
    station: &mut Socket,
    peer: &mut Peer,
    tx: i32,
) -> Vec<(String, Value)> {
    let mut results = Vec::new();
    for (index, (rate, unit)) in [("0", "A"), ("900719925474099.1", "W")]
        .into_iter()
        .enumerate()
    {
        let mut payload = profile(1, -117, rate, unit);
        payload["csChargingProfiles"]["chargingProfilePurpose"] = json!("TxProfile");
        payload["csChargingProfiles"]["transactionId"] = json!(tx);
        let body = command(
            &format!("profile-{index}"),
            1,
            "SetChargingProfile",
            payload,
        );
        let pending = begin(client, fixture, body.clone());
        peer.serve_one(station).await;
        let value = completed(pending).await;
        assert_eq!(value["schema_version"], json!({"major":1,"revision":6}));
        assert_eq!(value["lifecycle"]["accepted"], true);
        let request = &value["charging_profile_16"]["request"];
        assert_eq!(request["cs_charging_profiles"]["transaction_id"], tx);
        assert_eq!(request["cs_charging_profiles"]["charging_profile_id"], -117);
        assert_eq!(
            request["cs_charging_profiles"]["charging_schedule"]["charging_schedule_period"][0]["limit"],
            rate
        );
        assert_eq!(
            request["cs_charging_profiles"]["charging_schedule"]["charging_schedule_period"][0]["number_phases"],
            4
        );
        assert_eq!(
            request["cs_charging_profiles"]["charging_schedule"]["charging_schedule_period"][1]["start_period"],
            86401
        );
        assert!(value.get("observed_effects").is_none());
        assert_eq!(completed(begin(client, fixture, body)).await, value);
        no_call(station).await;
        assert_eq!(
            result(client, fixture, &format!("profile-{index}")).await,
            value
        );
        results.push((format!("profile-{index}"), value));
    }
    results
}

async fn verify_peer_replacements_and_clears(
    client: &reqwest::Client,
    fixture: &Fixture,
    station: &mut Socket,
    peer: &mut Peer,
) {
    // Charger replacement by same ID and by purpose/stack is peer behavior, not a bridge ledger.
    let replacement = begin(
        client,
        fixture,
        command(
            "replace-stack",
            1,
            "SetChargingProfile",
            profile(1, 118, "0", "A"),
        ),
    );
    peer.serve_one(station).await;
    let replace_result = completed(replacement).await;
    assert_eq!(
        replace_result["charging_profile_16"]["request"]["cs_charging_profiles"]["charging_profile_id"],
        118
    );
    assert_eq!(peer.profiles().len(), 2); // TxProfile and TxDefaultProfile are distinct purposes.
    let replacement = begin(
        client,
        fixture,
        command(
            "replace-purpose-stack",
            1,
            "SetChargingProfile",
            profile(1, 119, "8.1", "W"),
        ),
    );
    peer.serve_one(station).await;
    let replace_result = completed(replacement).await;
    assert_eq!(
        replace_result["charging_profile_16"]["request"]["cs_charging_profiles"]["charging_profile_id"],
        119
    );
    assert_eq!(
        peer.profiles()
            .iter()
            .filter(|profile| profile["csChargingProfiles"]["chargingProfileId"] == 118)
            .count(),
        0
    );
    let clear = begin(
        client,
        fixture,
        command(
            "id-overrides",
            0,
            "ClearChargingProfile",
            json!({"id":119,"connectorId":999,"stackLevel":0,"chargingProfilePurpose":"ChargePointMaxProfile"}),
        ),
    );
    peer.serve_one(station).await;
    let clear = completed(clear).await;
    assert_eq!(clear["charging_profile_16"]["status"], "Accepted");
    assert_eq!(clear["charging_profile_16"]["request"]["connector_id"], 999);
    let unknown = begin(
        client,
        fixture,
        command(
            "clear-unknown",
            1,
            "ClearChargingProfile",
            json!({"connectorId":1,"stackLevel":2,"chargingProfilePurpose":"TxDefaultProfile"}),
        ),
    );
    peer.serve_one(station).await;
    assert_eq!(
        completed(unknown).await["charging_profile_16"]["status"],
        "Unknown"
    );
}

#[tokio::test]
async fn native_validity_kind_recurrence_and_station_scope_are_preserved() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    for (index, kind) in ["Absolute", "Relative", "Recurring"]
        .into_iter()
        .enumerate()
    {
        let mut payload = profile(0, i32::MIN, "0", "W");
        payload["csChargingProfiles"]["chargingProfilePurpose"] = json!("ChargePointMaxProfile");
        payload["csChargingProfiles"]["chargingProfileKind"] = json!(kind);
        if kind == "Recurring" {
            payload["csChargingProfiles"]["recurrencyKind"] = json!("Weekly");
        } else {
            payload["csChargingProfiles"]
                .as_object_mut()
                .unwrap()
                .remove("recurrencyKind");
        }
        let result = exchange(
            &client,
            &fixture,
            &mut station,
            command(&format!("kind-{index}"), 0, "SetChargingProfile", payload),
            "Accepted",
        )
        .await;
        assert_eq!(result["charging_profile_16"]["request"]["connector_id"], 0);
        assert_eq!(
            result["charging_profile_16"]["request"]["cs_charging_profiles"]["charging_profile_kind"],
            kind
        );
        assert_eq!(
            result["charging_profile_16"]["request"]["cs_charging_profiles"]["valid_from"],
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            result["charging_profile_16"]["request"]["cs_charging_profiles"]["valid_to"],
            "2099-01-01T00:00:00Z"
        );
    }
}
