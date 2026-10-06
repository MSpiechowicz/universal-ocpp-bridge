use std::os::unix::fs::PermissionsExt;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uob_sim::reservation16::{
    ConnectorState, ReservationConfig, ReservationHandle, ReserveRequest, ReserveStatus,
};

fn now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-10-05T00:00:00Z", &Rfc3339).unwrap()
}
fn request(id: i32, connector: u32) -> ReserveRequest {
    serde_json::from_value(serde_json::json!({"connectorId":connector,"expiryDate":"2026-10-05T00:01:00Z","idTag":"native-owner-113","parentIdTag":"native-group-113","reservationId":id})).unwrap()
}

#[test]
fn native_outcomes_follow_actual_station_state_not_configured_fake_reply() {
    let model = ReservationHandle::in_memory("alpha", &[1, 2], true, false);
    assert_eq!(
        model.reserve(request(-113, 0), now()).unwrap(),
        ReserveStatus::Rejected
    );
    assert_eq!(
        model.reserve(request(-113, 3), now()).unwrap(),
        ReserveStatus::Rejected
    );
    for (state, expected) in [
        (ConnectorState::Faulted, ReserveStatus::Faulted),
        (ConnectorState::Unavailable, ReserveStatus::Unavailable),
        (ConnectorState::Occupied, ReserveStatus::Occupied),
    ] {
        model.set_connector(1, state).unwrap();
        assert_eq!(model.reserve(request(-113, 1), now()).unwrap(), expected);
    }
    model.set_connector(1, ConnectorState::Available).unwrap();
    assert_eq!(
        model.reserve(request(-113, 1), now()).unwrap(),
        ReserveStatus::Accepted
    );
    assert_eq!(
        model.reserve(request(0, 1), now()).unwrap(),
        ReserveStatus::Occupied
    );
    assert!(!model.cancel(0, now()).unwrap());
    assert!(model.cancel(-113, now()).unwrap());
    assert!(!model.cancel(-113, now()).unwrap());
    let disabled = ReservationHandle::in_memory("alpha", &[1], false, false);
    assert_eq!(
        disabled.reserve(request(1, 1), now()).unwrap(),
        ReserveStatus::Rejected
    );
}

#[test]
fn same_id_replaces_atomically_and_rejection_keeps_previous_connector() {
    let model = ReservationHandle::in_memory("alpha", &[1, 2], true, true);
    assert_eq!(
        model.reserve(request(i32::MIN, 1), now()).unwrap(),
        ReserveStatus::Accepted
    );
    assert_eq!(
        model.reserve(request(i32::MIN, 2), now()).unwrap(),
        ReserveStatus::Accepted
    );
    assert_eq!(model.snapshot()["reservations"][0]["connectorId"], 2);
    model.set_connector(1, ConnectorState::Faulted).unwrap();
    assert_eq!(
        model.reserve(request(i32::MIN, 1), now()).unwrap(),
        ReserveStatus::Faulted
    );
    assert_eq!(model.snapshot()["reservations"][0]["connectorId"], 2);
    assert_eq!(
        model
            .start(2, "NATIVE-OWNER-113", None, true, now())
            .unwrap(),
        Some(i32::MIN)
    );
    assert_eq!(model.snapshot()["activeReservations"], 0);
}

#[test]
fn groups_are_actual_parent_facts_not_authorization_or_parent_as_token() {
    let model = ReservationHandle::in_memory("alpha", &[1, 2], true, true);
    model.reserve(request(0, 1), now()).unwrap();
    assert!(
        model
            .start(1, "native-group-113", None, true, now())
            .is_err()
    );
    assert!(
        model
            .start(1, "other", Some("native-group-113"), false, now())
            .is_err()
    );
    assert!(
        model
            .start(1, "other", Some("different-group"), true, now())
            .is_err()
    );
    assert_eq!(model.snapshot()["activeReservations"], 1);
    assert_eq!(
        model
            .start(2, "native-owner-113", None, true, now())
            .unwrap(),
        None
    );
    assert_eq!(model.snapshot()["activeReservations"], 1);
    assert_eq!(
        model
            .start(1, "other", Some("NATIVE-GROUP-113"), true, now())
            .unwrap(),
        Some(0)
    );
}

#[test]
fn any_keeps_capacity_unbound_and_a_single_failed_child_is_not_station_failure() {
    let model = ReservationHandle::in_memory("alpha", &[1, 2], true, true);
    model.reserve(request(i32::MAX, 0), now()).unwrap();
    assert_eq!(model.snapshot()["reservations"][0]["connectorId"], 0);
    model.set_connector(1, ConnectorState::Faulted).unwrap();
    assert_eq!(model.snapshot()["activeReservations"], 1);
    assert!(model.start(2, "unrelated", None, true, now()).is_err());
    assert_eq!(
        model
            .start(2, "native-owner-113", None, true, now())
            .unwrap(),
        Some(i32::MAX)
    );
    assert_eq!(model.snapshot()["activeReservations"], 0);
}

#[test]
fn inclusive_expiry_and_fault_unavailable_terminate_without_transaction() {
    let model = ReservationHandle::in_memory("alpha", &[1, 2], true, true);
    model.reserve(request(-113, 1), now()).unwrap();
    assert!(
        model
            .expire(now() + time::Duration::seconds(59))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        model.expire(now() + time::Duration::seconds(60)).unwrap(),
        vec![1]
    );
    model.reserve(request(0, 2), now()).unwrap();
    model.set_connector(2, ConnectorState::Unavailable).unwrap();
    assert_eq!(model.snapshot()["activeReservations"], 0);
    model.reserve(request(-113, 1), now()).unwrap();
    model.set_connector(0, ConnectorState::Faulted).unwrap();
    assert_eq!(model.snapshot()["activeReservations"], 0);
}

#[test]
fn native_semantic_boundaries_do_not_mutate_and_capacity_is_explicit() {
    let model = ReservationHandle::in_memory("alpha", &[1], true, true);
    for token in ["XXXXXXXXXXXXXXXXXXXXX", "native"] {
        let mut invalid = request(1, 1);
        invalid.id_tag = token.to_owned();
        if token == "native" {
            invalid.expiry_date = "invalid".to_owned();
        }
        assert!(model.reserve(invalid, now()).is_err());
    }
    assert_eq!(model.snapshot()["revision"], 0);
    let mut expired = request(1, 1);
    expired.expiry_date = "2026-10-05T00:00:00Z".to_owned();
    assert_eq!(
        model.reserve(expired, now()).unwrap(),
        ReserveStatus::Rejected
    );
    model.reserve(request(0, 0), now()).unwrap();
    assert_eq!(
        model.reserve(request(-1, 0), now()).unwrap(),
        ReserveStatus::Occupied
    );
}

#[test]
fn durable_private_restart_retains_real_reservation_and_cancelled_absence() {
    let directory = std::env::temp_dir().join(format!("uob-reservation-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::canonicalize(directory).unwrap();
    let config = ReservationConfig {
        private_state_file: directory.join("state.json").to_str().unwrap().to_owned(),
        enabled: true,
        reserve_connector_zero_supported: true,
    };
    let first = ReservationHandle::open("alpha", &[1, 2], &config).unwrap();
    first.reserve(request(-113, 0), now()).unwrap();
    let revision = first.snapshot()["revision"].clone();
    assert!(ReservationHandle::open("alpha", &[1, 2], &config).is_err());
    drop(first);
    let second = ReservationHandle::open("alpha", &[1, 2], &config).unwrap();
    assert_eq!(second.snapshot()["revision"], revision);
    assert_eq!(second.snapshot()["reservations"][0]["reservationId"], -113);
    assert!(second.cancel(-113, now()).unwrap());
    drop(second);
    let third = ReservationHandle::open("alpha", &[1, 2], &config).unwrap();
    assert_eq!(third.snapshot()["activeReservations"], 0);
    drop(third);
    assert!(ReservationHandle::open("foreign", &[1, 2], &config).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn bounded_native_capacity_rejects_without_evicting_another_reservation() {
    let connectors: Vec<_> = (1..=129).collect();
    let model = ReservationHandle::in_memory("alpha", &connectors, true, true);
    for connector in 1_u16..=128 {
        assert_eq!(
            model
                .reserve(request(i32::from(connector), u32::from(connector)), now())
                .unwrap(),
            ReserveStatus::Accepted
        );
    }
    assert_eq!(
        model.reserve(request(129, 129), now()).unwrap(),
        ReserveStatus::Rejected
    );
    assert_eq!(model.snapshot()["activeReservations"], 128);
    assert_eq!(
        model
            .start(1, "native-owner-113", None, true, now())
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        model.reserve(request(129, 129), now()).unwrap(),
        ReserveStatus::Accepted
    );
}
