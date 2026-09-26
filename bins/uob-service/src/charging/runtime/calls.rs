use std::io;

use serde_json::json;
use uob_application::{
    ChargerObservation, CommandClock, ObservationCommitError, OperationalStore,
    record_measurements, record_transaction_event, transaction16::TransactionContext,
};
use uob_contracts::{
    Connectivity, EventId, ProtocolEdition, ServiceIdentity, StationSnapshot, TargetInstanceId,
    UtcTimestamp,
};
use uob_protocol_adapter::{IncomingCall, OcppCallError, OcppErrorCode};

use super::{Clock, unavailable};
use crate::charging::ChargingStore;

pub(super) async fn apply_observation(
    incoming: &IncomingCall,
    snapshot: &mut StationSnapshot,
    store: &ChargingStore,
    identity: &ServiceIdentity,
    target: Option<(TargetInstanceId, u64)>,
    now: UtcTimestamp,
    committed: &mut Option<EventId>,
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
    let context = context(store, identity, incoming, target)
        .await
        .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?;
    if matches!(
        incoming.call.observation,
        ChargerObservation::TransactionEvent(_)
    ) {
        *committed = Some(context.event_id.clone());
    }
    let reply = match &incoming.call.observation {
        ChargerObservation::TransactionEvent(observation)
            if protocol == ProtocolEdition::Ocpp201 =>
        {
            record_transaction_event(store, snapshot, observation, context, now)
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
            record_measurements(store, snapshot, measurements, context, now)
                .await
                .map_err(|error| commit_error(protocol, &error))?;
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
