use super::{observe, read_key, save, transitions::sync_result};
use crate::configuration::unavailable;
use rusqlite::{Connection, TransactionBehavior};
use uob_application::{
    ReservationObservation201, ReservationObservationKind201, StorageError, reservation_live_201,
};
use uob_contracts::{ReservationState201, UtcTimestamp};

pub(crate) fn maintain(
    connection: &mut Connection,
    now: UtcTimestamp,
    startup: bool,
) -> Result<(), StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let stations = {
        let mut statement = transaction
            .prepare("SELECT DISTINCT station FROM reservations201")
            .map_err(unavailable)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?
    };
    for station in stations {
        let records = read_key(&transaction, &station)?;
        if startup {
            transaction
                .execute(
                    "UPDATE reservations201 SET inflight=0 WHERE station=?1",
                    [&station],
                )
                .map_err(unavailable)?;
            for mut record in records.iter().filter(|r| r.unresolved).cloned() {
                if reservation_live_201(record.state) {
                    record.state = if record.started {
                        ReservationState201::Uncertain
                    } else {
                        ReservationState201::Rejected
                    };
                    record.changed_at = record.changed_at.max(now);
                }
                record.unresolved = record.started;
                save(&transaction, &record, Some(false))?;
                sync_result(&transaction, &record)?;
            }
        }
        if let Some(record) = records.first() {
            observe(
                &transaction,
                &ReservationObservation201 {
                    station: record.station.clone(),
                    observed_at: now,
                    kind: ReservationObservationKind201::Expiry,
                },
            )?;
        }
        super::retention::prune(&transaction, &station, now)?;
    }
    transaction.commit().map_err(unavailable)
}
