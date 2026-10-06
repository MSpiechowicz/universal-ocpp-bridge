use std::io;

use super::{CallContext, Clock, CommitState, trigger, trigger201, unavailable};
use crate::charging::ChargingStore;
use serde_json::json;
use uob_application::{
    ChargerObservation, CommandClock, ObservationCommitError, OperationalStore,
    transaction16::TransactionContext,
};
use uob_contracts::{
    Connectivity, EventEnvelope, EventId, NativeProtocolReference, ProtocolEdition,
    ServiceIdentity, StationEvent, StationSnapshot, TargetInstanceId, TriggerMessageClass,
    TriggerMessageClass201, TriggerTarget201, UtcTimestamp,
};
use uob_protocol_adapter::{IncomingCall, OcppCallError, OcppErrorCode};

pub(super) async fn apply_observation(
    incoming: &IncomingCall,
    snapshot: &mut StationSnapshot,
    services: &mut CallContext<'_>,
    now: UtcTimestamp,
    commits: &mut CommitState,
) -> Result<serde_json::Value, OcppCallError> {
    let protocol = match &snapshot.connectivity {
        Connectivity::Connected { protocol, .. } => *protocol,
        _ => {
            return Err(call_error(
                ProtocolEdition::Ocpp16j,
                OcppErrorCode::InternalError,
            ));
        }
    };
    let marker = observation_marker(incoming, snapshot, services, protocol, now).await?;
    let context = context(
        services.store,
        services.identity,
        incoming,
        services.target.take(),
    )
    .await
    .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?;
    if matches!(
        incoming.call.observation,
        ChargerObservation::TransactionEvent(_)
    ) {
        commits.committed = Some(context.event_id.clone());
    }
    let reply = match &incoming.call.observation {
        ChargerObservation::TransactionEvent(observation)
            if protocol == ProtocolEdition::Ocpp201 =>
        {
            let triggered = marker.is_some();
            // Group membership comes only from the owner's provisioned identity list.
            let group = services.reservations_201.and_then(|provider| {
                observation
                    .reservation_token_key
                    .as_ref()
                    .and_then(|key| provider.group_key(key))
            });
            let outcome = uob_application::record_transaction_event_with_reservation(
                services.store,
                snapshot,
                observation,
                context,
                now,
                marker,
                group,
            )
            .await
            .map_err(|error| commit_error(protocol, &error))?;
            commits.trigger_committed =
                triggered && outcome == uob_application::TransactionApplyOutcome::Applied;
            if observation.event == uob_application::TransactionEventKind::Started
                && observation.id_token_present
            {
                // A transaction report is not evidence of an authorization grant.
                json!({"idTokenInfo":{"status":"Invalid"}})
            } else {
                json!({})
            }
        }
        ChargerObservation::Measurements(measurements) if measurements.protocol == protocol => {
            let trigger_committed = marker.is_some();
            uob_application::record_measurements_with_trigger(
                services.store,
                snapshot,
                measurements,
                context,
                now,
                marker,
            )
            .await
            .map_err(|error| commit_error(protocol, &error))?;
            commits.trigger_committed = trigger_committed;
            json!({})
        }
        _ => return Err(call_error(protocol, OcppErrorCode::NotImplemented)),
    };
    Ok(json!([3, incoming.call.message_id, reply]))
}

/// Commits the station's explicit reservation termination before acknowledging it.
pub(super) async fn reservation_status_update(
    incoming: &IncomingCall,
    snapshot: &StationSnapshot,
    services: &CallContext<'_>,
) -> Result<serde_json::Value, OcppCallError> {
    let protocol = ProtocolEdition::Ocpp201;
    let ChargerObservation::ReservationStatusUpdate201 {
        reservation_id,
        status,
    } = &incoming.call.observation
    else {
        return Err(call_error(protocol, OcppErrorCode::ProtocolError));
    };
    uob_application::record_reservation_status_201(
        services.store,
        snapshot,
        *reservation_id,
        *status,
        Clock.now(),
    )
    .await
    .map_err(|error| commit_error(protocol, &error))?;
    Ok(json!([3, incoming.call.message_id, {}]))
}

async fn observation_marker(
    incoming: &IncomingCall,
    snapshot: &StationSnapshot,
    services: &CallContext<'_>,
    protocol: ProtocolEdition,
    now: UtcTimestamp,
) -> Result<Option<EventEnvelope<StationEvent>>, OcppCallError> {
    let marker = if services.trigger_enabled && protocol == ProtocolEdition::Ocpp16j {
        if let ChargerObservation::Measurements(measurements) = &incoming.call.observation {
            let NativeProtocolReference::Ocpp16 { connector_id } = measurements.native_resource
            else {
                return Err(call_error(protocol, OcppErrorCode::ProtocolError));
            };
            if connector_id == 0 {
                None
            } else {
                trigger::marker(
                    services.store,
                    &snapshot.station,
                    services.identity,
                    TriggerMessageClass::MeterValues,
                    Some(connector_id),
                    None,
                    now,
                )
                .await
                .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?
            }
        } else {
            None
        }
    } else if services.trigger_enabled && protocol == ProtocolEdition::Ocpp201 {
        match &incoming.call.observation {
            ChargerObservation::Measurements(measurements) => {
                let NativeProtocolReference::Ocpp201 { evse_id, .. } = measurements.native_resource
                else {
                    return Err(call_error(protocol, OcppErrorCode::ProtocolError));
                };
                trigger201::marker(
                    services.store,
                    &snapshot.station,
                    services.identity,
                    TriggerMessageClass201::MeterValues,
                    TriggerTarget201::Evse { id: evse_id },
                    None,
                    now,
                )
                .await
                .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?
            }
            ChargerObservation::TransactionEvent(observation)
                if observation.trigger_reason == "Trigger" =>
            {
                let NativeProtocolReference::Ocpp201 {
                    evse_id,
                    connector_id,
                } = observation.native_resource
                else {
                    return Err(call_error(protocol, OcppErrorCode::ProtocolError));
                };
                let target = match connector_id {
                    Some(connector_id) => TriggerTarget201::Connector {
                        id: evse_id,
                        connector_id,
                    },
                    None => TriggerTarget201::Evse { id: evse_id },
                };
                trigger201::marker(
                    services.store,
                    &snapshot.station,
                    services.identity,
                    TriggerMessageClass201::TransactionEvent,
                    target,
                    Some("Trigger"),
                    now,
                )
                .await
                .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?
            }
            _ => None,
        }
    } else {
        None
    };
    Ok(marker)
}

pub(super) async fn context(
    store: &ChargingStore,
    identity: &ServiceIdentity,
    incoming: &IncomingCall,
    target: Option<(TargetInstanceId, u64)>,
) -> io::Result<TransactionContext> {
    let (sequence, event_id) = event_identity(store, identity).await?;
    Ok(TransactionContext {
        identity: identity.clone(),
        event_id,
        sequence,
        correlation_id: Some(incoming.correlation_id.clone()),
        target,
        delivery_deadline: UtcTimestamp::new(Clock.now().into_inner() + time::Duration::days(1)),
    })
}

pub(super) async fn event_identity(
    store: &ChargingStore,
    identity: &ServiceIdentity,
) -> io::Result<(u64, EventId)> {
    let sequence = store
        .reserve_event_sequence()
        .await
        .map_err(|_| unavailable())?;
    let event_id = EventId::new(format!("{}/event/{sequence}", identity.bridge_id.as_str()))
        .map_err(|_| unavailable())?;
    Ok((sequence, event_id))
}

pub(super) fn commit_error(
    protocol: ProtocolEdition,
    error: &ObservationCommitError,
) -> OcppCallError {
    let code = if matches!(error, ObservationCommitError::Storage(_)) {
        OcppErrorCode::InternalError
    } else {
        OcppErrorCode::ProtocolError
    };
    call_error(protocol, code)
}

pub(super) fn call_error(protocol: ProtocolEdition, code: OcppErrorCode) -> OcppCallError {
    OcppCallError {
        protocol,
        code,
        description: "Charging operation could not be completed",
        field_path: None,
    }
}
