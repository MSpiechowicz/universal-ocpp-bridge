mod task;

use std::{error::Error, fmt, time::Duration};

use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};
use uob_application::{AdmissionError, Application, RuntimeReservation, WorkClass};
use uob_contracts::{CorrelationId, ProtocolActionName, ProtocolEdition};

use super::frame;
use crate::{
    DecodedCall, OcppCallError, OcppErrorCode, v16::remote_control::DeferredConfigurationCall,
};

/// Per-connection queue and response bounds for the OCPP call lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallSessionConfiguration {
    pub pending_call_capacity: usize,
    pub incoming_call_capacity: usize,
    pub diagnostic_capacity: usize,
    pub response_timeout: Duration,
}

impl CallSessionConfiguration {
    /// Validates nonzero local bounds against the process-wide pending-request limit.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error before any task or queue is allocated.
    pub fn validate(
        self,
        application: &Application,
    ) -> Result<Self, CallSessionConfigurationError> {
        let limits = &application.health().resources().limits().queues;
        if self.pending_call_capacity == 0
            || self.incoming_call_capacity == 0
            || self.diagnostic_capacity == 0
            || self.response_timeout.is_zero()
            || self.pending_call_capacity > limits.pending_requests
            || self.incoming_call_capacity > limits.pending_requests
            || self.diagnostic_capacity > limits.diagnostics
        {
            return Err(CallSessionConfigurationError);
        }
        Ok(self)
    }
}

/// Invalid zero or process-limit-exceeding session configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallSessionConfigurationError;

impl fmt::Display for CallSessionConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid OCPP call session bounds")
    }
}

impl Error for CallSessionConfigurationError {}

/// One validated bridge-originated OCPP CALL.
#[derive(Clone, Eq, PartialEq)]
pub struct OutboundCall {
    pub message_id: String,
    pub action: ProtocolActionName,
    pub payload: Value,
    pub correlation_id: CorrelationId,
}
impl fmt::Debug for OutboundCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboundCall")
            .field("message_id", &self.message_id)
            .field("action", &self.action)
            .field("payload", &"[REDACTED]")
            .field("correlation_id", &self.correlation_id)
            .finish()
    }
}

/// Sanitized charger CALLERROR without arbitrary remote description or detail content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteCallError {
    pub code: String,
    pub field_path: Option<String>,
}

/// Why a sent command has no authoritative correlated reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransmissionUncertainReason {
    WriteFailed,
    Disconnected,
    SessionStopped,
}

/// Terminal result for one submitted outbound call.
#[derive(Clone, Eq, PartialEq)]
pub enum SessionCallOutcome {
    Result {
        payload: Value,
        correlation_id: CorrelationId,
    },
    Error {
        error: RemoteCallError,
        correlation_id: CorrelationId,
    },
    TimedOut {
        correlation_id: CorrelationId,
    },
    NotTransmitted {
        reason: &'static str,
        correlation_id: CorrelationId,
    },
    TransmissionUncertain {
        reason: TransmissionUncertainReason,
        correlation_id: CorrelationId,
    },
}
impl fmt::Debug for SessionCallOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Result { correlation_id, .. } => formatter
                .debug_struct("Result")
                .field("payload", &"[REDACTED]")
                .field("correlation_id", correlation_id)
                .finish(),
            Self::Error {
                error,
                correlation_id,
            } => formatter
                .debug_struct("Error")
                .field("error", error)
                .field("correlation_id", correlation_id)
                .finish(),
            Self::TimedOut { correlation_id } => formatter
                .debug_struct("TimedOut")
                .field("correlation_id", correlation_id)
                .finish(),
            Self::NotTransmitted {
                reason,
                correlation_id,
            } => formatter
                .debug_struct("NotTransmitted")
                .field("reason", reason)
                .field("correlation_id", correlation_id)
                .finish(),
            Self::TransmissionUncertain {
                reason,
                correlation_id,
            } => formatter
                .debug_struct("TransmissionUncertain")
                .field("reason", reason)
                .field("correlation_id", correlation_id)
                .finish(),
        }
    }
}

/// Failure to admit a call before any bytes can be transmitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionSubmitError {
    InvalidRequest,
    Resource(AdmissionError),
    Full,
    Closed,
}

/// Awaitable result identity returned after successful local admission.
pub struct PendingCall {
    receiver: oneshot::Receiver<SessionCallOutcome>,
    correlation_id: CorrelationId,
}

impl PendingCall {
    /// Waits for correlation, timeout, or conservative transport classification.
    pub async fn receive(self) -> SessionCallOutcome {
        self.receiver
            .await
            .unwrap_or(SessionCallOutcome::TransmissionUncertain {
                reason: TransmissionUncertainReason::SessionStopped,
                correlation_id: self.correlation_id,
            })
    }
}

pub(super) enum QueuedWire {
    Ready(String),
    Configuration(DeferredConfigurationCall),
}

pub(super) struct QueuedOutbound {
    pub request: OutboundCall,
    pub wire: QueuedWire,
    pub result: oneshot::Sender<SessionCallOutcome>,
    pub reservation: RuntimeReservation,
    pub send_before: Option<Instant>,
}

/// Nonblocking producer for bridge-originated calls.
#[derive(Clone)]
pub struct CallSessionHandle {
    pub(super) sender: mpsc::Sender<QueuedOutbound>,
    pub(super) budget: uob_application::RuntimeResourceBudget,
    pub(super) station_id: uob_contracts::StationId,
    pub(super) protocol: ProtocolEdition,
}

impl CallSessionHandle {
    /// Admits a call without waiting for queue space or a charger response.
    ///
    /// # Errors
    ///
    /// Rejects malformed, over-budget, full, or stopped submissions before transmission.
    pub fn try_call(&self, request: OutboundCall) -> Result<PendingCall, SessionSubmitError> {
        self.enqueue(request, None, None)
    }

    /// Admits work with a monotonic last-send deadline checked by the socket owner.
    /// # Errors
    /// Returns the same bounded admission failures as `try_call`.
    pub fn try_call_before(
        &self,
        request: OutboundCall,
        deadline: Instant,
    ) -> Result<PendingCall, SessionSubmitError> {
        self.enqueue(request, Some(deadline), None)
    }

    pub(crate) fn try_configuration_call_before(
        &self,
        request: OutboundCall,
        deadline: Instant,
        deferred: DeferredConfigurationCall,
    ) -> Result<PendingCall, SessionSubmitError> {
        self.enqueue(request, Some(deadline), Some(deferred))
    }

    /// Exact authenticated socket identity; a handle never follows a reconnect.
    #[must_use]
    pub fn station_id(&self) -> &uob_contracts::StationId {
        &self.station_id
    }

    /// Protocol negotiated by the authenticated socket.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolEdition {
        self.protocol
    }

    /// Whether the socket owner has stopped accepting work.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    fn enqueue(
        &self,
        request: OutboundCall,
        send_before: Option<Instant>,
        deferred: Option<DeferredConfigurationCall>,
    ) -> Result<PendingCall, SessionSubmitError> {
        if request.message_id.trim().is_empty() || !request.payload.is_object() {
            return Err(SessionSubmitError::InvalidRequest);
        }
        let (wire, bytes) = if let Some(deferred) = deferred {
            let bytes = deferred.wire_size(&request.message_id).unwrap_or_default();
            (QueuedWire::Configuration(deferred), bytes)
        } else {
            let encoded = frame::call(
                &request.message_id,
                request.action.as_str(),
                &request.payload,
            );
            let bytes = encoded.len();
            (QueuedWire::Ready(encoded), bytes)
        };
        self.budget
            .validate_ocpp_message(bytes)
            .map_err(SessionSubmitError::Resource)?;
        let reservation = self
            .budget
            .try_reserve(WorkClass::PendingRequest, bytes)
            .map_err(SessionSubmitError::Resource)?;
        let correlation_id = request.correlation_id.clone();
        let (result, receiver) = oneshot::channel();
        self.sender
            .try_send(QueuedOutbound {
                request,
                wire,
                result,
                reservation,
                send_before,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SessionSubmitError::Full,
                mpsc::error::TrySendError::Closed(_) => SessionSubmitError::Closed,
            })?;
        Ok(PendingCall {
            receiver,
            correlation_id,
        })
    }
}

pub(super) struct QueuedReply {
    pub trace: uob_application::FlowSpan,
    pub message_id: String,
    pub encoded: String,
    pub _reservation: RuntimeReservation,
}

/// Single-use response capability for a validated charger-originated call.
pub struct IncomingCallResponder {
    pub(super) trace: uob_application::FlowSpan,
    pub(super) message_id: String,
    pub(super) protocol: ProtocolEdition,
    pub(super) sender: Option<mpsc::Sender<QueuedReply>>,
    pub(super) budget: uob_application::RuntimeResourceBudget,
}

impl IncomingCallResponder {
    /// Queues an application result while the socket reader remains active.
    ///
    /// # Errors
    ///
    /// Rejects a response that exceeds shared byte limits or whose queue is unavailable.
    pub fn respond(mut self, payload: &Value) -> Result<(), SessionSubmitError> {
        let encoded = if payload.is_object() {
            frame::result(&self.message_id, payload)
        } else {
            frame::error(
                &self.message_id,
                OcppCallError {
                    protocol: self.protocol,
                    code: OcppErrorCode::InternalError,
                    description: "application response failed validation",
                    field_path: Some("/"),
                },
            )
        };
        self.send(encoded)
    }

    /// Queues a sanitized application CALLERROR.
    ///
    /// # Errors
    ///
    /// Rejects an error response that exceeds shared limits or whose queue is unavailable.
    pub fn reject(mut self, error: OcppCallError) -> Result<(), SessionSubmitError> {
        let encoded = frame::error(&self.message_id, error);
        self.send(encoded)
    }

    fn send(&mut self, encoded: String) -> Result<(), SessionSubmitError> {
        if self.sender.is_none() {
            return Ok(());
        }
        self.budget
            .validate_ocpp_message(encoded.len())
            .map_err(SessionSubmitError::Resource)?;
        let reservation = self
            .budget
            .try_reserve(WorkClass::CriticalReport, encoded.len())
            .map_err(SessionSubmitError::Resource)?;
        if let Some(sender) = self.sender.take() {
            sender
                .try_send(QueuedReply {
                    trace: self.trace.clone(),
                    message_id: self.message_id.clone(),
                    encoded,
                    _reservation: reservation,
                })
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => SessionSubmitError::Full,
                    mpsc::error::TrySendError::Closed(_) => SessionSubmitError::Closed,
                })?;
        }
        Ok(())
    }
}

impl Drop for IncomingCallResponder {
    fn drop(&mut self) {
        let protocol = self.protocol;
        let encoded = frame::error(
            &self.message_id,
            OcppCallError {
                protocol,
                code: OcppErrorCode::InternalError,
                description: "application response unavailable",
                field_path: None,
            },
        );
        let _ = self.send(encoded);
    }
}

/// Validated charger operation delivered only after envelope, direction, and schema checks.
pub struct IncomingCall {
    /// Process-local context carried across the socket/application queue.
    pub trace: uob_application::FlowSpan,
    pub call: DecodedCall,
    pub correlation_id: CorrelationId,
    pub responder: IncomingCallResponder,
    pub(super) _reservation: RuntimeReservation,
}

/// Consumer for bounded validated charger calls.
pub struct IncomingCallReceiver {
    pub(super) receiver: mpsc::Receiver<IncomingCall>,
}

impl IncomingCallReceiver {
    pub async fn receive(&mut self) -> Option<IncomingCall> {
        self.receiver.recv().await
    }
}

/// Sanitized sequencing evidence that is not associated with an active caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallSessionDiagnostic {
    MalformedFrame {
        message_id: Option<String>,
        field_path: &'static str,
    },
    DuplicateIncomingCall {
        message_id: String,
    },
    UnmatchedResponse {
        message_id: String,
    },
    LateResponse {
        message_id: String,
        correlation_id: CorrelationId,
    },
}

/// Bounded application and diagnostic consumers for one session.
pub struct CallSessionOutputs {
    pub incoming: IncomingCallReceiver,
    pub diagnostics: mpsc::Receiver<CallSessionDiagnostic>,
}

/// Owning task handle for one socket call lifecycle.
pub struct CallSessionTask {
    pub(super) shutdown: Option<oneshot::Sender<()>>,
    pub(super) join: Option<JoinHandle<()>>,
}

pub(super) struct PendingEntry {
    pub trace: uob_application::FlowSpan,
    pub result: oneshot::Sender<SessionCallOutcome>,
    pub correlation_id: CorrelationId,
    pub deadline: Instant,
    pub _reservation: RuntimeReservation,
}
