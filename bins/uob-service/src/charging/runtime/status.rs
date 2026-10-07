//! Status evidence is committed with the corresponding station state transition.
use std::io;

use uob_application::{CommandClock, registration::availability::AvailabilityContext};
use uob_contracts::{ProtocolEdition, StationSnapshot, TriggerMessageClass201, TriggerTarget201};
use uob_protocol_adapter::{DecodedCall, IncomingCall, OcppCallError, OcppErrorCode, v16, v201};

use super::{CallContext, Clock, CommitState, call_error, event_identity, trigger, trigger201};

pub(super) async fn complete_trigger_status(
    incoming: &IncomingCall,
    snapshot: &StationSnapshot,
    services: &CallContext<'_>,
    commits: &mut CommitState,
) -> io::Result<Result<serde_json::Value, OcppCallError>> {
    let protocol = ProtocolEdition::Ocpp16j;
    if uob_application::registration::accepted(snapshot).is_err() {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    }
    let uob_application::ChargerObservation::TriggerStatus { class, status } =
        &incoming.call.observation
    else {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    };
    let marker = if services.trigger_enabled {
        trigger::marker(
            services.store,
            &snapshot.station,
            services.identity,
            *class,
            None,
            Some(status),
            Clock.now(),
        )
        .await?
    } else {
        None
    };
    if let Some(marker) = marker {
        trigger::commit_status(services.store, marker).await?;
        commits.trigger_committed = true;
    }
    Ok(Ok(serde_json::json!([3, incoming.call.message_id, {}])))
}

/// The service has no log, firmware, or certificate workflow to process. Preserve only
/// validated native receipt evidence, then explicitly reject the unsupported workflow.
pub(super) async fn complete_trigger_receipt_201(
    incoming: &IncomingCall,
    snapshot: &StationSnapshot,
    services: &CallContext<'_>,
    commits: &mut CommitState,
) -> io::Result<Result<serde_json::Value, OcppCallError>> {
    let protocol = ProtocolEdition::Ocpp201;
    if uob_application::registration::v201::accepted(snapshot).is_err() {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    }
    let (class, status) = match &incoming.call.observation {
        uob_application::ChargerObservation::TriggerStatus201 { class, status } => {
            (*class, Some(status.as_str()))
        }
        uob_application::ChargerObservation::FirmwareStatus201 { status, .. } => (
            TriggerMessageClass201::FirmwareStatusNotification,
            Some(status.as_str()),
        ),
        uob_application::ChargerObservation::LogStatus201 { status, .. } => (
            TriggerMessageClass201::LogStatusNotification,
            Some(status.as_str()),
        ),
        uob_application::ChargerObservation::TriggerCertificate201 { class } => (*class, None),
        _ => return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError))),
    };
    let valid_class = matches!(
        class,
        TriggerMessageClass201::LogStatusNotification
            | TriggerMessageClass201::FirmwareStatusNotification
            | TriggerMessageClass201::PublishFirmwareStatusNotification
    ) || class.is_certificate();
    if !valid_class {
        return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
    }
    if services.trigger_enabled {
        let marker = trigger201::marker(
            services.store,
            &snapshot.station,
            services.identity,
            class,
            TriggerTarget201::Station,
            status,
            Clock.now(),
        )
        .await?;
        if let Some(marker) = marker {
            trigger::commit_status(services.store, marker).await?;
            commits.trigger_committed = true;
        }
    }
    Ok(Err(call_error(protocol, OcppErrorCode::NotImplemented)))
}

pub(super) async fn complete_status(
    call: DecodedCall,
    snapshot: &mut StationSnapshot,
    services: &CallContext<'_>,
    protocol: ProtocolEdition,
    commits: &mut CommitState,
) -> io::Result<Result<serde_json::Value, OcppCallError>> {
    let (sequence, event_id) = event_identity(services.store, services.identity).await?;
    commits.committed = Some(event_id.clone());
    let context = AvailabilityContext {
        identity: services.identity.clone(),
        event_id,
        sequence,
    };
    Ok(match protocol {
        ProtocolEdition::Ocpp16j => {
            let marker = if services.trigger_enabled {
                let uob_application::ChargerObservation::ConnectorStatus(status) =
                    &call.observation
                else {
                    return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
                };
                trigger::marker(
                    services.store,
                    &snapshot.station,
                    services.identity,
                    uob_contracts::TriggerMessageClass::StatusNotification,
                    Some(status.connector_id),
                    None,
                    Clock.now(),
                )
                .await?
            } else {
                None
            };
            let trigger_committed = marker.is_some();
            let response = v16::availability::complete_status_with_trigger(
                call,
                services.store,
                snapshot,
                context,
                Clock.now(),
                marker,
            )
            .await;
            commits.trigger_committed = trigger_committed && response.is_ok();
            response
        }
        ProtocolEdition::Ocpp201 => {
            let marker = if services.trigger_enabled {
                let uob_application::ChargerObservation::EvseConnectorStatus(status) =
                    &call.observation
                else {
                    return Ok(Err(call_error(protocol, OcppErrorCode::ProtocolError)));
                };
                trigger201::marker(
                    services.store,
                    &snapshot.station,
                    services.identity,
                    TriggerMessageClass201::StatusNotification,
                    TriggerTarget201::Connector {
                        id: status.evse_id,
                        connector_id: status.connector_id,
                    },
                    None,
                    Clock.now(),
                )
                .await?
            } else {
                None
            };
            let trigger_committed = marker.is_some();
            let response = v201::availability::complete_status_with_trigger(
                call,
                services.store,
                snapshot,
                context,
                Clock.now(),
                marker,
            )
            .await;
            commits.trigger_committed = trigger_committed && response.is_ok();
            response
        }
    })
}
