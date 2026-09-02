use std::{error::Error, fmt, time::Duration};

use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout},
};
use uob_application::{AdmissionError, Application, RuntimeReservation, WorkClass};
use uob_contracts::{CorrelationId, ProtocolActionName, ProtocolEdition};

use super::frame;
use crate::{DecodedCall, OcppCallError, OcppErrorCode};

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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundCall {
    pub message_id: String,
    pub action: ProtocolActionName,
    pub payload: Value,
    pub correlation_id: CorrelationId,
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
#[derive(Clone, Debug, Eq, PartialEq)]
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

pub(super) struct QueuedOutbound {
    pub request: OutboundCall,
    pub encoded: String,
    pub result: oneshot::Sender<SessionCallOutcome>,
    pub reservation: RuntimeReservation,
}

/// Nonblocking producer for bridge-originated calls.
#[derive(Clone)]
pub struct CallSessionHandle {
    pub(super) sender: mpsc::Sender<QueuedOutbound>,
    pub(super) budget: uob_application::RuntimeResourceBudget,
}

impl CallSessionHandle {
    /// Admits a call without waiting for queue space or a charger response.
    ///
    /// # Errors
    ///
    /// Rejects malformed, over-budget, full, or stopped submissions before transmission.
    pub fn try_call(&self, request: OutboundCall) -> Result<PendingCall, SessionSubmitError> {
        if request.message_id.trim().is_empty() || !request.payload.is_object() {
            return Err(SessionSubmitError::InvalidRequest);
        }
        let encoded = frame::call(
            &request.message_id,
            request.action.as_str(),
            &request.payload,
        );
        self.budget
            .validate_ocpp_message(encoded.len())
            .map_err(SessionSubmitError::Resource)?;
        let reservation = self
            .budget
            .try_reserve(WorkClass::PendingRequest, encoded.len())
            .map_err(SessionSubmitError::Resource)?;
        let correlation_id = request.correlation_id.clone();
        let (result, receiver) = oneshot::channel();
        self.sender
            .try_send(QueuedOutbound {
                request,
                encoded,
                result,
                reservation,
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
    pub message_id: String,
    pub encoded: String,
    pub _reservation: RuntimeReservation,
}

/// Single-use response capability for a validated charger-originated call.
pub struct IncomingCallResponder {
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

impl CallSessionTask {
    /// Waits for peer disconnection or another terminal socket condition.
    ///
    /// # Errors
    ///
    /// Reports an unavailable, cancelled, or panicked session task.
    pub async fn wait(mut self) -> Result<(), &'static str> {
        let Some(join) = self.join.take() else {
            return Err("session task unavailable");
        };
        join.await.map_err(|_| "session task failed")
    }

    /// Requests graceful socket close within a caller-supplied deadline.
    ///
    /// # Errors
    ///
    /// Reports an unavailable, failed, or deadline-exceeding session task.
    pub async fn shutdown(mut self, deadline: Duration) -> Result<(), &'static str> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let mut join = self.join.take().ok_or("session task unavailable")?;
        match timeout(deadline, &mut join).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("session task failed"),
            Err(_) => {
                join.abort();
                let _ = join.await;
                Err("session shutdown deadline exceeded")
            }
        }
    }
}

impl Drop for CallSessionTask {
    fn drop(&mut self) {
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}

pub(super) struct PendingEntry {
    pub result: oneshot::Sender<SessionCallOutcome>,
    pub correlation_id: CorrelationId,
    pub deadline: Instant,
    pub _reservation: RuntimeReservation,
}
