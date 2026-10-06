use uob_application::{
    ReservationCandidate201, ReservationKey201, ReservationUpdateStatus201, reservation_live_201,
    reservation_matches_201,
};
use uob_contracts::{ReservationState201, UtcTimestamp};

fn candidate(evse_id: Option<u32>) -> ReservationCandidate201 {
    ReservationCandidate201 {
        evse_id,
        connector_type: None,
        expiry_date_time: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
        token_key: ReservationKey201([1; 32]),
        group_key: Some(ReservationKey201([2; 32])),
    }
}
#[test]
fn identity_or_group_matching_honours_evse_scope_and_never_uses_the_group_as_a_token() {
    let exact = candidate(Some(3));
    let token = ReservationKey201([1; 32]);
    let group = ReservationKey201([2; 32]);
    let other = ReservationKey201([7; 32]);
    assert!(reservation_matches_201(&exact, 3, Some(&token), None));
    assert!(reservation_matches_201(
        &exact,
        3,
        Some(&other),
        Some(&group)
    ));
    assert!(
        !reservation_matches_201(&exact, 3, Some(&group), None),
        "the group key presented as a token is not the reserved identity"
    );
    assert!(!reservation_matches_201(
        &exact,
        3,
        Some(&other),
        Some(&ReservationKey201([9; 32]))
    ));
    assert!(!reservation_matches_201(&exact, 4, Some(&token), None));
    assert!(
        reservation_matches_201(&exact, 3, None, None),
        "a tokenless event relies on the station's explicit reservationId"
    );
    assert!(!reservation_matches_201(&exact, 4, None, None));
    let unspecified = candidate(None);
    assert!(reservation_matches_201(&unspecified, 4, Some(&token), None));
    assert!(reservation_matches_201(&unspecified, 9, None, None));
    let mut ungrouped = candidate(None);
    ungrouped.group_key = None;
    assert!(!reservation_matches_201(
        &ungrouped,
        1,
        Some(&other),
        Some(&group)
    ));
    assert!(!format!("{exact:?}").contains("[1, 1"));
    assert!(!format!("{exact:?}").contains("[2, 2"));
}
#[test]
fn only_pending_active_and_uncertain_are_live_and_native_updates_are_terminal() {
    for state in [
        ReservationState201::Consumed,
        ReservationState201::Cancelled,
        ReservationState201::Expired,
        ReservationState201::Removed,
        ReservationState201::Rejected,
        ReservationState201::Superseded,
        ReservationState201::Ambiguous,
    ] {
        assert!(!reservation_live_201(state));
    }
    for state in [
        ReservationState201::Pending,
        ReservationState201::Active,
        ReservationState201::Uncertain,
    ] {
        assert!(reservation_live_201(state));
    }
    assert_eq!(
        ReservationUpdateStatus201::Expired.state(),
        ReservationState201::Expired
    );
    assert_eq!(
        ReservationUpdateStatus201::Removed.state(),
        ReservationState201::Removed
    );
    assert!(!reservation_live_201(
        ReservationUpdateStatus201::Removed.state()
    ));
}
