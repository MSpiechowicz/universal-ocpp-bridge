use crate::charging::{ChargingStore, runtime::Clock};
use std::io;
use uob_application::{AtomicStoreWrite, CommandClock, CommandDispatchOutcome, OperationalStore};
use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandLifecycle, CommandOperation, CommandResult,
    Connectivity, ContractVersion, ProtocolEdition, RequestId, StationSnapshot,
};
use uob_protocol_adapter::CallSessionDiagnostic;

pub(crate) async fn late_response(
    store: &ChargingStore,
    snapshot: &StationSnapshot,
    diagnostic: CallSessionDiagnostic,
) -> io::Result<()> {
    let (message_id, correlation_id, action, status) = match diagnostic {
        CallSessionDiagnostic::LateReservationResponse16 {
            message_id,
            correlation_id,
            action,
            status,
        } => (
            message_id,
            correlation_id,
            action,
            serde_json::to_value(status).map_err(|_| invalid())?,
        ),
        CallSessionDiagnostic::LateReservationResponse201 {
            message_id,
            correlation_id,
            action,
            status,
        } => (
            message_id,
            correlation_id,
            action,
            serde_json::to_value(status).map_err(|_| invalid())?,
        ),
        _ => return Ok(()),
    };
    let request = RequestId::new(message_id).map_err(|_| invalid())?;
    let Some(command) = store
        .command_by_request_id(request.clone())
        .await
        .map_err(|_| invalid())?
    else {
        return Ok(());
    };
    let Connectivity::Connected { connected_at, .. } = snapshot.connectivity else {
        return Ok(());
    };
    if command.resource.bridge_id != snapshot.station.bridge_id
        || command.resource.station_id != snapshot.station.station_id
        || command.admitted_at < connected_at
        || command
            .correlation_id
            .as_ref()
            .map_or(command.request_id.as_str(), |id| id.as_str())
            != correlation_id.as_str()
    {
        return Ok(());
    }
    let Some(mut result) = store
        .command_result_by_request_id(request)
        .await
        .map_err(|_| invalid())?
    else {
        return Ok(());
    };
    let payload = serde_json::json!({"status":status});
    let Some(accepted) = native_evidence(&mut result, &command, action, &payload) else {
        return Ok(());
    };
    result.recorded_at = Clock.now();
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted,
        error: (!accepted).then_some(CommandError {
            code: CommandErrorCode::ProtocolRejected,
            detail: None,
        }),
    };
    let mut write = AtomicStoreWrite::empty();
    write.purpose = uob_application::StorageWritePurpose::ActiveSessionCompletion;
    write.command_result = Some(result);
    store.write_atomic(write).await.map_err(|_| invalid())?;
    Ok(())
}
/// Records the edition's value-free evidence; `None` leaves the uncertain result untouched.
fn native_evidence(
    result: &mut CommandResult,
    command: &Command<serde_json::Value>,
    action: &str,
    payload: &serde_json::Value,
) -> Option<bool> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return None;
    };
    match operation.protocol {
        ProtocolEdition::Ocpp16j => {
            let CommandDispatchOutcome::ReservationResponse16(evidence) =
                uob_protocol_adapter::v16::remote_control::reservation_response_16(
                    action,
                    payload,
                    command,
                    Clock.now(),
                )
            else {
                return None;
            };
            result.schema_version = ContractVersion::V1_RESERVATION_16;
            let accepted = evidence.accepted();
            result.reservation_16 = Some(evidence);
            Some(accepted)
        }
        ProtocolEdition::Ocpp201 => {
            let CommandDispatchOutcome::ReservationResponse201(evidence) =
                uob_protocol_adapter::v201::remote_control::reservation_response_201(
                    action,
                    payload,
                    command,
                    Clock.now(),
                )
            else {
                return None;
            };
            result.schema_version = ContractVersion::V1_RESERVATION_201;
            let accepted = evidence.accepted();
            result.reservation_201 = Some(evidence);
            Some(accepted)
        }
    }
}
fn invalid() -> io::Error {
    io::Error::other("late reservation acknowledgement persistence unavailable")
}
