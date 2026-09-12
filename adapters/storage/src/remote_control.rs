use crate::{SqliteOperationalStore, configuration::unavailable, worker::Request};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use uob_application::remote_control::{RemoteControlEvidence, RemoteControlStore};
use uob_application::{StorageError, StorageErrorCode, StorageFuture};
use uob_contracts::RequestId;

pub(crate) enum Operation {
    Reserve(RequestId),
    Response(RequestId, String, Option<String>),
    Read(RequestId),
}
pub(crate) enum Outcome {
    Reserved(i32),
    Recorded,
    Evidence(Option<RemoteControlEvidence>),
}
impl<C, E, D, R> RemoteControlStore for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + 'static,
    E: DeserializeOwned + Send + 'static,
    D: DeserializeOwned + Send + 'static,
    R: DeserializeOwned + Send + 'static,
{
    fn reserve_remote_start(&self, request: RequestId) -> StorageFuture<'_, i32> {
        Box::pin(async move {
            match self
                .request(|reply| Request::RemoteControl(Operation::Reserve(request), reply))
                .await?
            {
                Outcome::Reserved(id) => Ok(id),
                _ => Err(invalid()),
            }
        })
    }
    fn record_remote_response(
        &self,
        request: RequestId,
        status: String,
        transaction: Option<String>,
    ) -> StorageFuture<'_, ()> {
        Box::pin(async move {
            match self
                .request(|reply| {
                    Request::RemoteControl(Operation::Response(request, status, transaction), reply)
                })
                .await?
            {
                Outcome::Recorded => Ok(()),
                _ => Err(invalid()),
            }
        })
    }
    fn remote_control_evidence(
        &self,
        request: RequestId,
    ) -> StorageFuture<'_, Option<RemoteControlEvidence>> {
        Box::pin(async move {
            match self
                .request(|reply| Request::RemoteControl(Operation::Read(request), reply))
                .await?
            {
                Outcome::Evidence(value) => Ok(value),
                _ => Err(invalid()),
            }
        })
    }
}
pub(crate) fn apply(
    connection: &mut Connection,
    operation: &Operation,
) -> Result<Outcome, StorageError> {
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let request = match operation {
        Operation::Reserve(id) | Operation::Response(id, ..) | Operation::Read(id) => id,
    };
    let existing: Option<String> = tx
        .query_row(
            "SELECT payload FROM remote_control_evidence WHERE request_id = ?1",
            [request.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    let mut evidence: RemoteControlEvidence = existing
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| invalid())?
        .unwrap_or_default();
    if matches!(operation, Operation::Read(_)) {
        return Ok(Outcome::Evidence(existing.map(|_| evidence)));
    }
    // Only admitted commands may acquire a row. Allocation cannot grow an independent backlog.
    let present: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM commands WHERE request_id = ?1)",
            [request.as_str()],
            |r| r.get(0),
        )
        .map_err(unavailable)?;
    if !present {
        return Err(invalid());
    }
    let outcome = match operation {
        Operation::Reserve(_) => {
            let id = match evidence.remote_start_id {
                Some(id) => id,
                None => tx.query_row("UPDATE remote_start_counter SET value = value + 1 WHERE id = 1 AND value < 2147483647 RETURNING value", [], |r| r.get(0)).map_err(unavailable)?,
            };
            evidence.remote_start_id = Some(id);
            Outcome::Reserved(id)
        }
        Operation::Response(_, status, native) => {
            if !matches!(
                status.as_str(),
                "Accepted"
                    | "Rejected"
                    | "Scheduled"
                    | "Unlocked"
                    | "UnlockFailed"
                    | "OngoingAuthorizedTransaction"
                    | "UnknownConnector"
            ) || native
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.chars().count() > 36)
            {
                return Err(invalid());
            }
            if evidence.response_status.is_some()
                && (evidence.response_status.as_ref() != Some(status)
                    || &evidence.native_transaction_id != native)
            {
                return Err(invalid());
            }
            evidence.response_status = Some(status.clone());
            evidence.native_transaction_id.clone_from(native);
            Outcome::Recorded
        }
        Operation::Read(_) => unreachable!(),
    };
    tx.execute("INSERT INTO remote_control_evidence(request_id, payload) VALUES (?1, ?2) ON CONFLICT(request_id) DO UPDATE SET payload = excluded.payload", params![request.as_str(), serde_json::to_string(&evidence).map_err(|_| invalid())?]).map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    Ok(outcome)
}
fn invalid() -> StorageError {
    StorageError::new(
        StorageErrorCode::InvalidRequest,
        "invalid native remote command evidence",
    )
}
