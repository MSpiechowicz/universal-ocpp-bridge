use serde_json::{Value, json};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uob_sim::local_authorization201::IdToken;
use uob_sim::reservation201::{
    ConnectorStatus, ConnectorTypeDefinition, Reservation201Handle, Reservation201Policy,
    ReserveNowRequest, ReserveStatus, StatusUpdate, UpdateStatus,
};

fn now() -> OffsetDateTime {
    OffsetDateTime::parse("2026-10-06T00:00:00Z", &Rfc3339).unwrap()
}
fn request(id: i32, fields: &Value) -> ReserveNowRequest {
    let mut value = json!({"id":id,"expiryDateTime":"2026-10-06T00:01:00Z",
        "idToken":{"idToken":"native-owner-114","type":"ISO14443"},
        "groupIdToken":{"idToken":"native-group-114","type":"Central"}});
    value
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}
fn token(text: &str, kind: &str) -> IdToken {
    serde_json::from_value(json!({"idToken":text,"type":kind})).unwrap()
}
fn policy(non_evse_specific: bool) -> Reservation201Policy {
    Reservation201Policy {
        enabled: true,
        non_evse_specific,
        connector_types: vec![],
    }
}
fn model(evses: &[(u16, u16)], policy: &Reservation201Policy) -> Reservation201Handle {
    Reservation201Handle::in_memory("alpha", evses, policy).unwrap()
}

#[test]
fn native_statuses_follow_actual_evse_state_and_configuration() {
    let disabled = model(&[(1, 1)], &Reservation201Policy::default());
    assert_eq!(
        disabled.reserve(request(1, &json!({"evseId":1})), now()),
        Ok(ReserveStatus::Rejected),
        "H01.FR.01 not configured to accept reservations"
    );
    let station = model(&[(1, 1), (2, 1)], &policy(false));
    assert_eq!(
        station.reserve(request(1, &json!({})), now()),
        Ok(ReserveStatus::Rejected),
        "H01.FR.19 unspecified EVSE without NonEvseSpecific"
    );
    for evse in [0, 3, -1] {
        assert_eq!(
            station.reserve(request(1, &json!({ "evseId": evse })), now()),
            Ok(ReserveStatus::Rejected)
        );
    }
    for (status, expected) in [
        (ConnectorStatus::Faulted, ReserveStatus::Faulted),
        (ConnectorStatus::Unavailable, ReserveStatus::Unavailable),
        (ConnectorStatus::Occupied, ReserveStatus::Occupied),
    ] {
        station.set_connector(1, 1, status).unwrap();
        assert_eq!(
            station.reserve(request(1, &json!({"evseId":1})), now()),
            Ok(expected)
        );
    }
    station
        .set_connector(1, 1, ConnectorStatus::Available)
        .unwrap();
    assert_eq!(
        station.reserve(request(1, &json!({"evseId":1})), now()),
        Ok(ReserveStatus::Accepted)
    );
    assert_eq!(
        station.reserve(request(2, &json!({"evseId":1})), now()),
        Ok(ReserveStatus::Occupied),
        "errata H01.FR.11: a Reserved EVSE answers Occupied"
    );
    let expired = request(
        3,
        &json!({"evseId":2,"expiryDateTime":"2026-10-06T00:00:00Z"}),
    );
    assert_eq!(station.reserve(expired, now()), Ok(ReserveStatus::Rejected));
    assert_eq!(station.snapshot()["activeReservations"], 1);
}

#[test]
fn same_id_replaces_atomically_and_refused_replacement_keeps_previous_evse() {
    let station = model(&[(1, 1), (2, 1)], &policy(true));
    assert_eq!(
        station.reserve(request(i32::MIN, &json!({"evseId":1})), now()),
        Ok(ReserveStatus::Accepted)
    );
    assert_eq!(
        station.reserve(request(i32::MIN, &json!({"evseId":2})), now()),
        Ok(ReserveStatus::Accepted),
        "H01.FR.02 same id replaces"
    );
    assert_eq!(station.snapshot()["reservations"][0]["evseId"], 2);
    station
        .set_connector(1, 1, ConnectorStatus::Faulted)
        .unwrap();
    assert_eq!(
        station.reserve(request(i32::MIN, &json!({"evseId":1})), now()),
        Ok(ReserveStatus::Faulted)
    );
    assert_eq!(station.snapshot()["reservations"][0]["evseId"], 2);
    assert_eq!(station.snapshot()["activeReservations"], 1);
    assert!(station.updates().is_empty());
}

#[test]
fn reported_views_follow_exact_and_saturated_unspecified_reservations() {
    let station = model(&[(1, 1), (1, 2), (2, 1)], &policy(true));
    station
        .reserve(request(7, &json!({"evseId":1})), now())
        .unwrap();
    // FR.23 errata: every connector of the reserved EVSE reports Reserved.
    assert_eq!(
        station.take_statuses(),
        vec![((1, 1), "Reserved"), ((1, 2), "Reserved")]
    );
    assert_eq!(
        station.reserve(request(8, &json!({})), now()),
        Ok(ReserveStatus::Accepted)
    );
    // FR.20: one unspecified reservation for the one remaining free EVSE reserves it.
    assert_eq!(station.take_statuses(), vec![((2, 1), "Reserved")]);
    assert_eq!(
        station.reserve(request(9, &json!({})), now()),
        Ok(ReserveStatus::Occupied)
    );
    assert!(station.cancel(7, now()).unwrap());
    // EVSE 1 is free again, so no EVSE is critical for the single unspecified reservation.
    let mut views = station.take_statuses();
    views.sort_unstable();
    assert_eq!(
        views,
        vec![
            ((1, 1), "Available"),
            ((1, 2), "Available"),
            ((2, 1), "Available")
        ]
    );
    assert!(!station.cancel(7, now()).unwrap(), "H02.FR.01 unknown id");
    assert!(
        station.updates().is_empty(),
        "H02 remark: no status update for CSMS cancellation"
    );
}

#[test]
fn connector_type_reservations_count_only_matching_connectors() {
    let typed = Reservation201Policy {
        enabled: true,
        non_evse_specific: true,
        connector_types: vec![
            ConnectorTypeDefinition {
                evse: 1,
                connector: 1,
                connector_type: "cType2".to_owned(),
            },
            ConnectorTypeDefinition {
                evse: 2,
                connector: 1,
                connector_type: "cCCS2".to_owned(),
            },
        ],
    };
    let station = model(&[(1, 1), (2, 1)], &typed);
    assert_eq!(
        station.reserve(request(1, &json!({"connectorType":"cG105"})), now()),
        Ok(ReserveStatus::Rejected)
    );
    assert_eq!(
        station.reserve(
            request(1, &json!({"evseId":1,"connectorType":"cCCS2"})),
            now()
        ),
        Ok(ReserveStatus::Rejected)
    );
    assert_eq!(
        station.reserve(request(1, &json!({"connectorType":"cCCS2"})), now()),
        Ok(ReserveStatus::Accepted)
    );
    // FR.24: the only cCCS2 EVSE is now Reserved; cType2 stays usable.
    assert_eq!(station.take_statuses(), vec![((2, 1), "Reserved")]);
    assert_eq!(
        station.reserve(request(2, &json!({"connectorType":"cCCS2"})), now()),
        Ok(ReserveStatus::Occupied)
    );
    let owner = token("NATIVE-OWNER-114", "ISO14443");
    assert_eq!(
        station.start(1, 1, Some((&owner, None)), true, now()),
        Ok(None),
        "H03.FR.02 a different connector type does not end the reservation"
    );
    assert_eq!(
        station.start(2, 1, Some((&owner, None)), true, now()),
        Ok(Some(1))
    );
    assert!(
        Reservation201Handle::in_memory(
            "alpha",
            &[(1, 1)],
            &Reservation201Policy {
                connector_types: vec![ConnectorTypeDefinition {
                    evse: 1,
                    connector: 1,
                    connector_type: "Type2".to_owned()
                }],
                ..Reservation201Policy::default()
            }
        )
        .is_err()
    );
}

#[test]
fn inclusive_expiry_reports_available_before_expired() {
    let station = model(&[(1, 1)], &policy(false));
    station
        .reserve(request(-114, &json!({"evseId":1})), now())
        .unwrap();
    let _ = station.take_statuses();
    let expiry = OffsetDateTime::parse("2026-10-06T00:01:00Z", &Rfc3339).unwrap();
    station.expire(expiry - Duration::milliseconds(1)).unwrap();
    assert_eq!(station.snapshot()["activeReservations"], 1);
    station.expire(expiry).unwrap();
    assert_eq!(station.snapshot()["activeReservations"], 0);
    assert_eq!(station.take_statuses(), vec![((1, 1), "Available")]);
    let expired = StatusUpdate {
        reservation_id: -114,
        reservation_update_status: UpdateStatus::Expired,
    };
    assert_eq!(station.updates(), vec![expired]);
    station.acknowledge(expired).unwrap();
    assert!(station.updates().is_empty());
}

#[test]
fn faulted_or_unavailable_target_removes_and_unspecified_capacity_is_preserved() {
    let station = model(&[(1, 1), (2, 1), (3, 1)], &policy(true));
    station
        .reserve(request(10, &json!({"evseId":1})), now())
        .unwrap();
    station.reserve(request(11, &json!({})), now()).unwrap();
    station.reserve(request(12, &json!({})), now()).unwrap();
    station
        .set_connector(1, 1, ConnectorStatus::Faulted)
        .unwrap();
    station
        .set_connector(2, 1, ConnectorStatus::Unavailable)
        .unwrap();
    // H01.FR.16/17: the targeted EVSE and the unspecified reservation without capacity go.
    let removed = |id| StatusUpdate {
        reservation_id: id,
        reservation_update_status: UpdateStatus::Removed,
    };
    assert_eq!(station.updates(), vec![removed(10), removed(12)]);
    assert_eq!(station.snapshot()["activeReservations"], 1);
    assert_eq!(station.snapshot()["reservations"][0]["id"], 11);
    assert_eq!(
        station.set_connector(3, 1, ConnectorStatus::Occupied),
        Err("reservation_capacity_reserved")
    );
}

#[test]
fn matching_identity_or_group_ends_reservation_and_mismatch_is_denied() {
    let station = model(&[(1, 1), (2, 1)], &policy(true));
    station
        .reserve(request(20, &json!({"evseId":1})), now())
        .unwrap();
    station.reserve(request(21, &json!({})), now()).unwrap();
    let stranger = token("stranger-114", "ISO14443");
    let group = token("NATIVE-GROUP-114", "Central");
    let wrong_kind = token("native-owner-114", "Central");
    assert_eq!(
        station.start(1, 1, Some((&stranger, None)), true, now()),
        Err("reservation_identity_mismatch")
    );
    assert_eq!(
        station.start(1, 1, Some((&wrong_kind, None)), true, now()),
        Err("reservation_identity_mismatch"),
        "idToken type is part of the identity"
    );
    assert_eq!(
        station.start(1, 1, Some((&group, None)), true, now()),
        Err("reservation_identity_mismatch"),
        "a group token is not the reserved idToken"
    );
    assert_eq!(
        station.start(1, 1, None, false, now()),
        Err("reservation_identity_required")
    );
    assert_eq!(
        station.start(1, 1, Some((&stranger, Some(&group))), false, now()),
        Err("reservation_local_authorization_denied")
    );
    assert_eq!(
        station.start(1, 1, Some((&stranger, Some(&group))), true, now()),
        Ok(Some(20)),
        "H03.FR.04 matching groupIdToken"
    );
    let owner = token("Native-Owner-114", "ISO14443");
    assert_eq!(
        station.start(2, 1, Some((&owner, None)), true, now()),
        Ok(Some(21)),
        "H03.FR.03 case-insensitive idToken on any EVSE"
    );
    assert_eq!(station.snapshot()["activeReservations"], 0);
}
