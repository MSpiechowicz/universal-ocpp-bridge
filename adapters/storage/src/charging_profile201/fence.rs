use super::metadata::decode_mutation;
use crate::{codec, configuration::unavailable};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use uob_application::StorageError;
use uob_contracts::{CommandError, CommandErrorCode, CommandLifecycle};

/// Must run under the composition root's exclusive state-directory lock, not on arbitrary store open.
pub(crate) fn recover(connection: &mut Connection) -> Result<(), StorageError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    loop {
        let requests = {
            let mut statement = transaction
                .prepare("SELECT rowid FROM charging_profile201_mutations ORDER BY rowid LIMIT 128")
                .map_err(unavailable)?;
            statement
                .query_map([], |row| row.get::<_, i64>(0))
                .map_err(unavailable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(unavailable)?
        };
        if requests.is_empty() {
            break;
        }
        for rowid in requests {
            retire(&transaction, rowid)?;
        }
    }
    transaction.commit().map_err(unavailable)
}

pub(super) fn retire(transaction: &Transaction<'_>, rowid: i64) -> Result<(), StorageError> {
    let value = transaction
        .query_row(
            "SELECT request_id,station,started,payload FROM charging_profile201_mutations WHERE rowid=?1",
            [rowid],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?, row.get::<_, String>(3)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((request, station, started, payload)) = value else {
        return Ok(());
    };
    let _ = decode_mutation(&payload)?;
    if started {
        transaction
            .execute(
                "UPDATE charging_profile201_footprints SET state=2 WHERE station=?1 AND owner=?2",
                params![station, &request],
            )
            .map_err(unavailable)?;
    } else {
        transaction.execute("DELETE FROM charging_profile201_footprints WHERE station=?1 AND owner=?2 AND state=0",
            params![station, &request]).map_err(unavailable)?;
    }
    transaction
        .execute(
            "DELETE FROM charging_profile201_mutations WHERE rowid=?1",
            [rowid],
        )
        .map_err(unavailable)?;
    if let Some(payload) = transaction
        .query_row(
            "SELECT payload FROM command_results WHERE request_id=?1",
            [&request],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(unavailable)?
    {
        let mut result = codec::decode_result(&payload)?;
        result.lifecycle = if started {
            CommandLifecycle::TransmissionUncertain {
                detail: "profile mutation interrupted; explicit reconciliation required".to_owned(),
            }
        } else {
            CommandLifecycle::Rejected {
                error: CommandError {
                    code: CommandErrorCode::StationDisconnected,
                    detail: Some("profile mutation interrupted before dispatch".to_owned()),
                },
            }
        };
        result.charging_profile_201 = None;
        crate::command::write_result_value(transaction, result, &request)?;
    }
    Ok(())
}
