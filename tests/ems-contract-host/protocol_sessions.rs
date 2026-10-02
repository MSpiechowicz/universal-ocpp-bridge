use super::Sessions;
use serde_json::Value;
use uob_application::{
    CommandDispatchOutcome, StationCommandContext, StationCommandError, StationCommandFuture,
    StationCommandPort,
};
use uob_contracts::{CommandOperation, ResourceRef, UtcTimestamp};

impl StationCommandPort<Value> for Sessions {
    fn context(
        &self,
        resource: ResourceRef,
    ) -> StationCommandFuture<'_, Option<StationCommandContext>> {
        Box::pin(async move {
            let session = self
                .0
                .read()
                .await
                .get(resource.station_id.as_str())
                .cloned();
            if let Some(session) = session {
                session.port.context(resource).await
            } else {
                Ok(None)
            }
        })
    }
    fn charging_profile_expectation(
        &self,
        command: &uob_contracts::Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<uob_application::ProfileReservation201>, uob_contracts::CommandErrorCode>
    {
        let sessions = self
            .0
            .try_read()
            .map_err(|_| uob_contracts::CommandErrorCode::PolicyRejected)?;
        let session = sessions
            .get(command.resource.station_id.as_str())
            .ok_or(uob_contracts::CommandErrorCode::StationDisconnected)?;
        // This fixture's memory store does not implement native201 profile ownership.
        if session
            .port
            .charging_profile_expectation(command, generation, now)?
            .is_some()
        {
            return Err(uob_contracts::CommandErrorCode::UnsupportedOperation);
        }
        Ok(None)
    }
    fn dispatch(
        &self,
        command: uob_contracts::Command<Value>,
    ) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        Box::pin(async move {
            let session = self
                .0
                .read()
                .await
                .get(command.resource.station_id.as_str())
                .cloned();
            let mut session =
                session.ok_or_else(|| StationCommandError::new("station disconnected"))?;
            if matches!(&command.operation, CommandOperation::Stop { .. }) {
                // The store commits the start before the charger receives its response.
                // Its next Heartbeat is sent only after the simulator records that response.
                while !*session.stop_ready.borrow_and_update() {
                    session
                        .stop_ready
                        .changed()
                        .await
                        .map_err(|_| StationCommandError::new("station disconnected"))?;
                }
            }
            session.port.dispatch(command).await
        })
    }
}
