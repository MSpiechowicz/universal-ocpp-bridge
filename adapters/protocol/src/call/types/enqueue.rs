use super::super::frame;
use super::{
    CallSessionHandle, OutboundCall, PendingCall, QueuedOutbound, QueuedWire, SessionSubmitError,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use uob_application::WorkClass;
use uob_contracts::ProtocolEdition;
impl CallSessionHandle {
    pub(super) fn enqueue(
        &self,
        request: OutboundCall,
        send_before: Option<Instant>,
        deferred: Option<QueuedWire>,
    ) -> Result<PendingCall, SessionSubmitError> {
        self.enqueue_observed(request, send_before, deferred, None)
    }
    pub(in crate::call) fn enqueue_observed(
        &self,
        request: OutboundCall,
        send_before: Option<Instant>,
        deferred: Option<QueuedWire>,
        dispatched: Option<oneshot::Sender<Instant>>,
    ) -> Result<PendingCall, SessionSubmitError> {
        let guarded = self.protocol == ProtocolEdition::Ocpp201
            && [
                "GetVariables",
                "GetBaseReport",
                "GetReport",
                "SetVariables",
                "SetNetworkProfile",
            ]
            .contains(&request.action.as_str());
        if request.message_id.trim().is_empty()
            || !request.payload.is_object()
            || (guarded && request.message_id.len() > 256)
        {
            return Err(SessionSubmitError::InvalidRequest);
        }
        let (wire, bytes) = match deferred {
            Some(QueuedWire::Configuration(deferred)) => {
                let bytes = deferred.wire_size(&request.message_id).unwrap_or_default();
                (QueuedWire::Configuration(deferred), bytes)
            }
            Some(QueuedWire::Configuration201(deferred)) => {
                let bytes = deferred
                    .wire_size(&request.message_id)
                    .ok_or(SessionSubmitError::InvalidRequest)?;
                (QueuedWire::Configuration201(deferred), bytes)
            }
            Some(QueuedWire::Ready(_)) => return Err(SessionSubmitError::InvalidRequest),
            None => {
                // Native protected writes must never take the eager/raw-payload path.
                if self.protocol == ProtocolEdition::Ocpp201
                    && ["SetVariables", "SetNetworkProfile"].contains(&request.action.as_str())
                {
                    return Err(SessionSubmitError::InvalidRequest);
                }
                let encoded = frame::call(
                    &request.message_id,
                    request.action.as_str(),
                    &request.payload,
                );
                let bytes = encoded.len();
                (QueuedWire::Ready(encoded), bytes)
            }
        };
        if ["SetChargingProfile", "ClearChargingProfile"].contains(&request.action.as_str())
            && bytes > 256 * 1024
        {
            return Err(SessionSubmitError::InvalidRequest);
        }
        self.budget
            .validate_ocpp_message(bytes)
            .map_err(SessionSubmitError::Resource)?;
        let reservation = self
            .budget
            .try_reserve(WorkClass::PendingRequest, bytes)
            .map_err(SessionSubmitError::Resource)?;
        let correlation_id = request.correlation_id.clone();
        let (result, receiver) = oneshot::channel();
        let (response_reservation, retained) = if guarded {
            let (sender, receiver) = oneshot::channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        self.sender
            .try_send(QueuedOutbound {
                request,
                wire,
                result,
                reservation,
                send_before,
                dispatched,
                response_reservation,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SessionSubmitError::Full,
                mpsc::error::TrySendError::Closed(_) => SessionSubmitError::Closed,
            })?;
        Ok(PendingCall {
            receiver,
            correlation_id,
            response_reservation: retained,
        })
    }
}
