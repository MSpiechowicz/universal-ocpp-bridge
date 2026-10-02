//! Once authenticated profile admission begins, disconnecting HTTP does not abandon finalization.
use serde_json::Value;
use std::sync::Arc;
use uob_application::{
    CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
};
use uob_contracts::{CommandOperation, CommandResult, ExternalCommand, ProtocolEdition};

pub(crate) struct Supervisor(pub Arc<dyn CommandAdmissionPort<Value>>);
impl CommandAdmissionPort<Value> for Supervisor {
    fn submit(&self, command: ExternalCommand<Value>) -> CommandAdmissionFuture<'_, CommandResult> {
        let profile = matches!(
            &command.request.operation,
            CommandOperation::SetChargingLimit(_)
        ) || matches!(&command.request.operation, CommandOperation::Ocpp(operation)
                if operation.protocol == ProtocolEdition::Ocpp201
                    && ["SetChargingProfile", "ClearChargingProfile"].contains(&operation.action.as_str()));
        if !profile {
            return self.0.submit(command);
        }
        let inner = self.0.clone();
        let task = tokio::spawn(async move { inner.submit(command).await });
        Box::pin(async move {
            task.await.map_err(|_| {
                CommandAdmissionError::new(
                    CommandAdmissionErrorCode::Unavailable,
                    "profile dispatch supervisor stopped",
                )
            })?
        })
    }
}
