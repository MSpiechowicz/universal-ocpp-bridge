use std::io;

use super::{CallContext, Clock, CommitState, trigger, unavailable};
use crate::charging::ChargingStore;
use serde_json::json;
use uob_application::{
    ChargerObservation, CommandClock, ObservationCommitError, OperationalStore,
    record_transaction_event, transaction16::TransactionContext,
};
use uob_contracts::{
    Connectivity, EventId, NativeProtocolReference, ProtocolEdition, ServiceIdentity,
    StationSnapshot, TargetInstanceId, TriggerMessageClass, UtcTimestamp,
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
    } else {
        None
    };
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
            record_transaction_event(services.store, snapshot, observation, context, now)
                .await
                .map_err(|error| commit_error(protocol, &error))?;
            if observation.event == uob_application::TransactionEventKind::Started {
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

fn commit_error(protocol: ProtocolEdition, error: &ObservationCommitError) -> OcppCallError {
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
