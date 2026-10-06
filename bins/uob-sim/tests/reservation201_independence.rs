//! Expectations are hand-authored from OCA 2.0.1 Part 2 §H and Errata v1.0 §10 prose,
//! never produced by bridge types or encoders.
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uob_sim::local_authorization201::IdToken;
use uob_sim::reservation201::{
    ConnectorStatus, Reservation201Handle, Reservation201Policy, ReserveNowRequest, StatusUpdate,
    UpdateStatus,
};

fn native(frame: &Value) -> ReserveNowRequest {
    assert_eq!(
        (frame[0].clone(), frame[2].clone()),
        (json!(2), json!("ReserveNow"))
    );
    serde_json::from_value(frame[3].clone()).unwrap()
}

#[test]
fn hand_authored_wire_oracle_exercises_real_model_state_and_private_group_separation() {
    // S2 (specific EVSE), then S1 (unspecified EVSE) with a group, as on the native wire.
    let specific = json!([2,"oracle-1","ReserveNow",{"id":-114,"expiryDateTime":"2026-10-06T01:00:00Z","evseId":1,
        "idToken":{"idToken":"token-marker-114","type":"ISO14443"},"groupIdToken":{"idToken":"group-marker-114","type":"Central"}}]);
    let unspecified = json!([2,"oracle-2","ReserveNow",{"id":2_147_483_647,"expiryDateTime":"2026-10-06T01:00:00Z",
        "idToken":{"idToken":"other-marker-114","type":"KeyCode"}}]);
    let statuses = ["Accepted", "Faulted", "Occupied", "Rejected", "Unavailable"];
    let now = OffsetDateTime::parse("2026-10-06T00:00:00Z", &Rfc3339).unwrap();
    let model = Reservation201Handle::in_memory(
        "independent",
        &[(1, 1), (2, 1)],
        &Reservation201Policy {
            enabled: true,
            non_evse_specific: true,
            connector_types: vec![],
        },
    )
    .unwrap();
    model.set_connector(1, 1, ConnectorStatus::Faulted).unwrap();
    let faulted = serde_json::to_value(model.reserve(native(&specific), now).unwrap()).unwrap();
    assert_eq!(faulted, "Faulted");
    model
        .set_connector(1, 1, ConnectorStatus::Available)
        .unwrap();
    for frame in [&specific, &unspecified] {
        let status = serde_json::to_value(model.reserve(native(frame), now).unwrap()).unwrap();
        assert!(statuses.contains(&status.as_str().unwrap()));
        assert_eq!(status, "Accepted");
    }
    let member: IdToken =
        serde_json::from_value(json!({"idToken":"member-marker-114","type":"ISO14443"})).unwrap();
    let group: IdToken =
        serde_json::from_value(json!({"idToken":"GROUP-MARKER-114","type":"Central"})).unwrap();
    let parent_as_token: IdToken =
        serde_json::from_value(json!({"idToken":"group-marker-114","type":"Central"})).unwrap();
    assert!(
        model
            .start(1, 1, Some((&parent_as_token, None)), true, now)
            .is_err()
    );
    assert!(
        model
            .start(1, 1, Some((&member, Some(&group))), false, now)
            .is_err()
    );
    assert_eq!(
        model.start(1, 1, Some((&member, Some(&group))), true, now),
        Ok(Some(-114))
    );
    // The remaining EVSE now carries the unspecified reservation: nobody else may occupy it.
    assert_eq!(
        model.set_connector(2, 1, ConnectorStatus::Occupied),
        Err("reservation_capacity_reserved")
    );
    model
        .set_connector(2, 1, ConnectorStatus::Unavailable)
        .unwrap();
    assert_eq!(
        model.updates(),
        vec![StatusUpdate {
            reservation_id: 2_147_483_647,
            reservation_update_status: UpdateStatus::Removed
        }]
    );
    let public = model.snapshot().to_string();
    for raw in [
        "token-marker-114",
        "group-marker-114",
        "member-marker-114",
        "other-marker-114",
    ] {
        assert!(!public.contains(raw));
    }
}

#[test]
fn simulator_crate_has_no_bridge_dependency() {
    let manifest = include_str!("../Cargo.toml");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("[lints]")
        .next()
        .unwrap();
    assert!(
        !dependencies.contains("uob-") && !dependencies.contains("uob_"),
        "independent station model must not link bridge crates"
    );
}
