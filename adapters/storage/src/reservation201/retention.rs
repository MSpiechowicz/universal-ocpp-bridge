use super::read_key;
use crate::configuration::unavailable;
use rusqlite::{Connection, params};
use uob_application::{OPERATIONAL_HISTORY_RETENTION_SECONDS, StorageError, reservation_live_201};
use uob_contracts::UtcTimestamp;

/// Never discard live/unresolved state or its still-retained public result. Keep the
/// newest revision as a monotonic station head even when every reservation is settled.
pub(super) fn prune(
    connection: &Connection,
    station: &str,
    now: UtcTimestamp,
) -> Result<(), StorageError> {
    let mut values = read_key(connection, station)?;
    let Some(mut head) = values.pop() else {
        return Ok(());
    };
    let mut history_floor = head.history_floor;
    let floor = UtcTimestamp::new(now.into_inner().saturating_sub(time::Duration::seconds(
        OPERATIONAL_HISTORY_RETENTION_SECONDS,
    )));
    for record in values {
        if record.unresolved || reservation_live_201(record.state) || record.changed_at > floor {
            continue;
        }
        let removed = connection
            .execute(
                "DELETE FROM reservations201 WHERE station=?1 AND revision=?2 AND inflight=0
            AND NOT EXISTS(SELECT 1 FROM command_results WHERE request_id=?3)",
                params![
                    station,
                    i64::try_from(record.revision)
                        .map_err(|_| super::conflict("reservation revision exhausted"))?,
                    record.request_id.as_str()
                ],
            )
            .map_err(unavailable)?;
        if removed == 1
            && record.started
            && record.candidate.is_some()
            && record.state != uob_contracts::ReservationState201::Rejected
        {
            history_floor =
                Some(history_floor.map_or(record.changed_at, |old| old.max(record.changed_at)));
        }
    }
    if history_floor != head.history_floor {
        head.history_floor = history_floor;
        super::save(connection, &head, None)?;
    }
    Ok(())
}
