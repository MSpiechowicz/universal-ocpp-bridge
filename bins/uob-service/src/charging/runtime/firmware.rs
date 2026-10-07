//! Native firmware notifications of both editions. The job fact and any pending trigger
//! evidence commit in one write before the empty CALLRESULT is released.
use std::io;

use uob_application::{
    AtomicStoreWrite, ChargerObservation, CommandClock, FirmwareObservation16,
    FirmwareObservation201, FirmwareObservationKind16, FirmwareObservationKind201,
    OperationalStore,
};
use uob_contracts::{
    FirmwareStatus16, ProtocolEdition, StationSnapshot, TriggerMessageClass,
    TriggerMessageClass201, TriggerTarget201,
};
use uob_protocol_adapter::{IncomingCall, OcppCallError, OcppErrorCode};

use super::{CallContext, Clock, CommitState, call_error, trigger, trigger201, unavailable};

pub(super) async fn complete_status(
    incoming: &IncomingCall,
    snapshot: &StationSnapshot,
    services: &CallContext<'_>,
    commits: &mut CommitState,
) -> io::Result<Result<serde_json::Value, OcppCallError>> {
    let protocol = ProtocolEdition::Ocpp16j;
    if uob_application::registration::accepted(snapshot).is_err() {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    }
    let (status, signed, request_id, trigger_status) = match &incoming.call.observation {
        ChargerObservation::TriggerStatus {
            class: TriggerMessageClass::FirmwareStatusNotification,
            status,
        } => {
            let Ok(native) = serde_json::from_value::<FirmwareStatus16>(status.as_str().into())
            else {
                return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
            };
            (native, false, None, Some(status.as_str()))
        }
        ChargerObservation::SignedFirmwareStatus16 { status, request_id } => {
            (*status, true, *request_id, None)
        }
        _ => return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError))),
    };
    // The Security Whitepaper message exists only where its workflow was explicitly enabled.
    if signed && !services.firmware.is_some_and(|firmware| firmware.signed) {
        return Ok(Err(call_error(protocol, OcppErrorCode::NotImplemented)));
    }
    let now = Clock.now();
    let mut write = AtomicStoreWrite::empty();
    if services.firmware.is_some() {
        write.firmware_observations_16.push(FirmwareObservation16 {
            station: snapshot.station.clone(),
            observed_at: now,
            kind: FirmwareObservationKind16::Status {
                status,
                signed,
                request_id,
            },
        });
    }
    if let Some(status) = trigger_status.filter(|_| services.trigger_enabled)
        && let Some(marker) = trigger::marker(
            services.store,
            &snapshot.station,
            services.identity,
            TriggerMessageClass::FirmwareStatusNotification,
            None,
            Some(status),
            now,
        )
        .await?
    {
        write.journal_events.push(marker);
        commits.trigger_committed = true;
    }
    if !write.firmware_observations_16.is_empty() || !write.journal_events.is_empty() {
        services
            .store
            .write_atomic(write)
            .await
            .map_err(|_| unavailable())?;
    }
    Ok(Ok(serde_json::json!([3, incoming.call.message_id, {}])))
}

/// OCPP 2.0.1 `FirmwareStatusNotification` for a firmware-enabled station. The decoder already
/// refused a non-`Idle` report without `requestId` (L01.FR.20); an unmatched `requestId` changes
/// no job but is still acknowledged, and still counts as trigger evidence.
pub(super) async fn complete_status_201(
    incoming: &IncomingCall,
    snapshot: &StationSnapshot,
    services: &CallContext<'_>,
    commits: &mut CommitState,
) -> io::Result<Result<serde_json::Value, OcppCallError>> {
    let protocol = ProtocolEdition::Ocpp201;
    if uob_application::registration::v201::accepted(snapshot).is_err() {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    }
    let ChargerObservation::FirmwareStatus201 { status, request_id } = incoming.call.observation
    else {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    };
    let now = Clock.now();
    let mut write = AtomicStoreWrite::empty();
    write
        .firmware_observations_201
        .push(FirmwareObservation201 {
            station: snapshot.station.clone(),
            observed_at: now,
            kind: FirmwareObservationKind201::Status { status, request_id },
        });
    if services.trigger_enabled
        && let Some(marker) = trigger201::marker(
            services.store,
            &snapshot.station,
            services.identity,
            TriggerMessageClass201::FirmwareStatusNotification,
            TriggerTarget201::Station,
            Some(status.as_str()),
            now,
        )
        .await?
    {
        write.journal_events.push(marker);
        commits.trigger_committed = true;
    }
    services
        .store
        .write_atomic(write)
        .await
        .map_err(|_| unavailable())?;
    Ok(Ok(serde_json::json!([3, incoming.call.message_id, {}])))
}
