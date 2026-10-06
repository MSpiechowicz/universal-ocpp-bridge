use uob_application::{
    ReservationCandidate16, ReservationKey16, reservation_live_16, reservation_matches_16,
};
use uob_contracts::{ReservationState16, UtcTimestamp};

fn candidate(connector_id: u32) -> ReservationCandidate16 {
    ReservationCandidate16 {
        connector_id,
        expiry_date: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
        token_key: ReservationKey16([1; 32]),
        group_key: Some(ReservationKey16([2; 32])),
    }
}
#[test]
fn direct_and_real_parent_to_parent_matching_never_authorize_parent_as_token() {
    let candidate = candidate(3);
    assert!(reservation_matches_16(
        &candidate,
        3,
        &ReservationKey16([1; 32]),
        None
    ));
    assert!(reservation_matches_16(
        &candidate,
        3,
        &ReservationKey16([7; 32]),
        Some(&ReservationKey16([2; 32]))
    ));
    assert!(!reservation_matches_16(
        &candidate,
        3,
        &ReservationKey16([2; 32]),
        None
    ));
    assert!(!reservation_matches_16(
        &candidate,
        3,
        &ReservationKey16([7; 32]),
        Some(&ReservationKey16([9; 32]))
    ));
    assert!(!reservation_matches_16(
        &candidate,
        4,
        &ReservationKey16([1; 32]),
        None
    ));
    assert!(reservation_matches_16(
        &super_candidate(),
        4,
        &ReservationKey16([1; 32]),
        None
    ));
    assert!(!format!("{candidate:?}").contains("[1, 1"));
    assert!(!format!("{candidate:?}").contains("[2, 2"));
}
fn super_candidate() -> ReservationCandidate16 {
    candidate(0)
}
#[test]
fn terminal_states_never_become_live_from_a_late_ack() {
    for state in [
        ReservationState16::Consumed,
        ReservationState16::Cancelled,
        ReservationState16::Expired,
        ReservationState16::Faulted,
        ReservationState16::Unavailable,
        ReservationState16::Rejected,
        ReservationState16::Superseded,
    ] {
        assert!(!reservation_live_16(state));
    }
    for state in [
        ReservationState16::Pending,
        ReservationState16::Active,
        ReservationState16::Uncertain,
    ] {
        assert!(reservation_live_16(state));
    }
}
