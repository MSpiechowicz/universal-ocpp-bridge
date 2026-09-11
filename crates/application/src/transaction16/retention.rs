use crate::registration;
use uob_contracts::{
    StationSnapshot, TransactionSnapshot, TransactionState, TypedValue, UtcTimestamp,
};
const FLOOR: &str = "ocpp16/transactions/replay_floor_seconds";
const WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;

/// Never lower the persisted replay floor, including after host clock correction.
pub(super) fn floor(snapshot: &StationSnapshot, now: UtcTimestamp) -> i64 {
    let current = now
        .into_inner()
        .unix_timestamp()
        .saturating_sub(WINDOW_SECONDS);
    snapshot
        .current_values
        .iter()
        .find_map(|v| {
            if v.point_id.as_str() == FLOOR
                && let Some(TypedValue::SignedInteger(value)) = v.value
            {
                return Some(value);
            }
            None
        })
        .map_or(current, |persisted| persisted.max(current))
}
fn retained(transaction: &TransactionSnapshot, floor: i64) -> bool {
    transaction.ocpp16.is_none()
        || transaction.state != TransactionState::Ended
        || transaction
            .ended_at
            .is_none_or(|t| t.into_inner().unix_timestamp() >= floor)
        || transaction.started_at.into_inner().unix_timestamp() >= floor
}
pub(super) fn count(snapshot: &StationSnapshot, now: UtcTimestamp) -> usize {
    let floor = floor(snapshot, now);
    snapshot
        .transactions
        .iter()
        .filter(|t| retained(t, floor))
        .count()
}
/// Pruning shares the lifecycle commit; the replay floor survives even if every old row expires.
pub(super) fn prune(snapshot: &mut StationSnapshot, now: UtcTimestamp) {
    let floor = floor(snapshot, now);
    snapshot.transactions.retain(|t| retained(t, floor));
    registration::set(
        &mut snapshot.current_values,
        FLOOR,
        Some(TypedValue::SignedInteger(floor)),
        None,
        now,
    );
}
