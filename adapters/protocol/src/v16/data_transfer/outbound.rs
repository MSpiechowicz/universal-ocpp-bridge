//! One-shot CSMS-originated OCPP 1.6J `DataTransfer` delivery.
use serde_json::{Value, json};
use uob_application::{
    OperationalStore,
    data_transfer::{
        Observation, OpaqueData, OutboundStatus, Registry, Reply, Status, commit_outbound,
    },
};
use uob_contracts::{
    CorrelationId, ProtocolActionName, ProtocolEdition, StationSnapshot, UtcTimestamp,
};

use crate::{
    CallSessionHandle, OcppCallError, OcppErrorCode, OutboundCall, RemoteCallError,
    SessionCallOutcome,
};

use super::super::PROTOCOL;
use super::{invalid_payload, persistence_error, registration_error};

/// Trusted context for one CSMS-originated `DataTransfer` call.
///
/// This is a low-level ordered-station-owner API, not a management ingress: its caller owns
/// authorization, request identity allocation, and selection of the authenticated socket.
pub struct OutboundRequest<'a> {
    /// Exact vendor/message request to deliver.
    pub request: &'a Observation,
    /// Fresh OCPP CALL message identity for this socket lifetime.
    pub message_id: &'a str,
    /// Correlation retained by the existing call lifecycle.
    pub correlation_id: CorrelationId,
}

/// Result of one non-replayed outbound `DataTransfer` attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutboundOutcome {
    /// A strict, correlated native `DataTransfer` response.
    Reply(Reply),
    /// The socket lifecycle elapsed before a correlated response.
    TimedOut,
    /// Bytes may have been sent without an authoritative response.
    TransmissionUncertain,
    /// The lifecycle proved bytes were not sent.
    NotTransmitted,
    /// A sanitized remote OCPP CALLERROR was received.
    Error(RemoteCallError),
}

/// Sends one CSMS-originated OCPP 1.6J `DataTransfer` without retrying it.
///
/// The adapter records a conservative durable `TransmissionUncertain` outcome before enqueueing
/// and a final sanitized outcome after the socket lifecycle terminates. It never stores the
/// vendor, message ID, request data, or response data. A reconnect gets no queued replay.
///
/// # Errors
///
/// Returns a sanitized local OCPP error when the socket/snapshot identity, registration, exact
/// locally installed capability, request shape, or durable outcome commit is invalid.
pub async fn send_data_transfer<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    handle: &CallSessionHandle,
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    registry: &Registry,
    context: OutboundRequest<'_>,
    now: UtcTimestamp,
) -> Result<OutboundOutcome, OcppCallError> {
    validate_context(handle, snapshot, registry, &context)?;
    commit_outbound(store, snapshot, OutboundStatus::TransmissionUncertain, now)
        .await
        .map_err(|_| persistence_error())?;

    let call = OutboundCall {
        message_id: context.message_id.to_owned(),
        action: ProtocolActionName::new("DataTransfer")
            .map_err(|_| invalid_payload().call_error())?,
        payload: request_payload(context.request),
        correlation_id: context.correlation_id,
    };
    let Ok(pending) = handle.try_call(call) else {
        commit_final(store, snapshot, OutboundStatus::NotTransmitted, now).await?;
        return Ok(OutboundOutcome::NotTransmitted);
    };
    let (outcome, status) = match pending.receive().await {
        SessionCallOutcome::Result { payload, .. } => match response_payload(payload) {
            Ok(reply) => {
                let status = reply.status;
                (OutboundOutcome::Reply(reply), OutboundStatus::Reply(status))
            }
            Err(()) => (
                OutboundOutcome::TransmissionUncertain,
                OutboundStatus::TransmissionUncertain,
            ),
        },
        SessionCallOutcome::Error { error, .. } => {
            (OutboundOutcome::Error(error), OutboundStatus::CallError)
        }
        SessionCallOutcome::TimedOut { .. } => {
            (OutboundOutcome::TimedOut, OutboundStatus::TimedOut)
        }
        SessionCallOutcome::NotTransmitted { .. } => (
            OutboundOutcome::NotTransmitted,
            OutboundStatus::NotTransmitted,
        ),
        SessionCallOutcome::TransmissionUncertain { .. } => (
            OutboundOutcome::TransmissionUncertain,
            OutboundStatus::TransmissionUncertain,
        ),
    };
    commit_final(store, snapshot, status, now).await?;
    Ok(outcome)
}

fn validate_context(
    handle: &CallSessionHandle,
    snapshot: &StationSnapshot,
    registry: &Registry,
    context: &OutboundRequest<'_>,
) -> Result<(), OcppCallError> {
    if handle.protocol() != ProtocolEdition::Ocpp16j
        || handle.station_id() != &snapshot.station.station_id
    {
        return Err(error(
            OcppErrorCode::ProtocolError,
            "Data transfer socket state is unavailable",
        ));
    }
    uob_application::registration::accepted(snapshot).map_err(|_| registration_error())?;
    context
        .request
        .validate()
        .map_err(|_| invalid_payload().call_error())?;
    if context.message_id.trim().is_empty() {
        return Err(invalid_payload().call_error());
    }
    if !registry.supports(context.request) {
        return Err(error(
            OcppErrorCode::NotImplemented,
            "Data transfer capability is not registered",
        ));
    }
    Ok(())
}

async fn commit_final<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    status: OutboundStatus,
    now: UtcTimestamp,
) -> Result<(), OcppCallError> {
    commit_outbound(store, snapshot, status, now)
        .await
        .map_err(|_| persistence_error())
}

fn request_payload(request: &Observation) -> Value {
    let mut payload = json!({"vendorId": request.vendor_id});
    if let Some(message_id) = &request.message_id {
        payload["messageId"] = Value::String(message_id.clone());
    }
    if let Some(data) = &request.data {
        payload["data"] = Value::String(data.expose().to_owned());
    }
    payload
}

fn response_payload(payload: Value) -> Result<Reply, ()> {
    let Value::Object(mut object) = payload else {
        return Err(());
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "status" | "data"))
    {
        return Err(());
    }
    let status = match object.get("status").and_then(Value::as_str) {
        Some("Accepted") => Status::Accepted,
        Some("Rejected") => Status::Rejected,
        Some("UnknownVendorId") => Status::UnknownVendorId,
        Some("UnknownMessageId") => Status::UnknownMessageId,
        _ => return Err(()),
    };
    let data = object
        .remove("data")
        .map(|value| match value {
            Value::String(text) => OpaqueData::new(text).map_err(|_| ()),
            _ => Err(()),
        })
        .transpose()?;
    if status == Status::UnknownVendorId && data.is_some() {
        return Err(());
    }
    Ok(Reply { status, data })
}

const fn error(code: OcppErrorCode, description: &'static str) -> OcppCallError {
    OcppCallError {
        protocol: PROTOCOL,
        code,
        description,
        field_path: None,
    }
}
