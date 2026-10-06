//! Expectations originate in the hand-authored OCA corpus, not bridge types or encoders.
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uob_sim::reservation16::{ConnectorState, ReservationHandle, ReserveRequest};

#[test]
fn hand_authored_wire_oracle_exercises_real_model_state_and_private_group_separation() {
    let wire: Value = serde_json::from_str(include_str!(
        "../../../tests/ocpp-fixtures/corpus/wire/1.6/reservation-exact-negative.json"
    ))
    .unwrap();
    let accepted: Value = serde_json::from_str(include_str!(
        "../../../tests/ocpp-fixtures/corpus/wire/1.6/reservation-accepted.json"
    ))
    .unwrap();
    let faulted: Value = serde_json::from_str(include_str!(
        "../../../tests/ocpp-fixtures/corpus/wire/1.6/reservation-faulted.json"
    ))
    .unwrap();
    let now = OffsetDateTime::parse("2026-10-05T00:00:00Z", &Rfc3339).unwrap();
    let model = ReservationHandle::in_memory("independent", &[1, 2], true, false);
    model.set_connector(1, ConnectorState::Faulted).unwrap();
    let request = serde_json::from_value::<ReserveRequest>(wire[3].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(model.reserve(request, now).unwrap()).unwrap(),
        faulted[2]["status"]
    );
    assert_eq!(model.snapshot()["activeReservations"], 0);
    model.set_connector(1, ConnectorState::Available).unwrap();
    let request = serde_json::from_value::<ReserveRequest>(wire[3].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(model.reserve(request, now).unwrap()).unwrap(),
        accepted[2]["status"]
    );
    assert!(model.start(1, "group-marker-113", None, true, now).is_err());
    assert!(
        model
            .start(1, "member-marker-113", Some("group-marker-113"), false, now)
            .is_err()
    );
    assert_eq!(
        model
            .start(1, "member-marker-113", Some("GROUP-MARKER-113"), true, now)
            .unwrap(),
        Some(-113)
    );
    let public = model.snapshot().to_string();
    for raw in ["token-marker-113", "group-marker-113", "member-marker-113"] {
        assert!(!public.contains(raw));
    }
}
