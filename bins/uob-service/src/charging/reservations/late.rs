use crate::charging::{ChargingStore, runtime::Clock};
use std::io;
use uob_application::{AtomicStoreWrite, CommandClock, CommandDispatchOutcome, OperationalStore};
use uob_contracts::{
    CommandError, CommandErrorCode, CommandLifecycle, Connectivity, ContractVersion, RequestId,
    StationSnapshot,
};
use uob_protocol_adapter::CallSessionDiagnostic;

pub(crate) async fn late_response(
    store: &ChargingStore,
    snapshot: &StationSnapshot,
    diagnostic: CallSessionDiagnostic,
) -> io::Result<()> {
    let CallSessionDiagnostic::LateReservationResponse16 {
        message_id,
        correlation_id,
        action,
        status,
    } = diagnostic
    else {
        return Ok(());
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
    let CommandDispatchOutcome::ReservationResponse16(evidence) =
        uob_protocol_adapter::v16::remote_control::reservation_response_16(
            action,
            &payload,
            &command,
            Clock.now(),
        )
    else {
        return Ok(());
    };
    let accepted = evidence.accepted();
    result.schema_version = ContractVersion::V1_RESERVATION_16;
    result.recorded_at = Clock.now();
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted,
        error: (!accepted).then_some(CommandError {
            code: CommandErrorCode::ProtocolRejected,
            detail: None,
        }),
    };
    result.reservation_16 = Some(evidence);
    let mut write = AtomicStoreWrite::empty();
    write.purpose = uob_application::StorageWritePurpose::ActiveSessionCompletion;
    write.command_result = Some(result);
    store.write_atomic(write).await.map_err(|_| invalid())?;
    Ok(())
}
fn invalid() -> io::Error {
    io::Error::other("late reservation acknowledgement persistence unavailable")
}
