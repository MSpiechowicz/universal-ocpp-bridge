//! Bounded bidirectional OCPP CALL lifecycle for one authenticated station socket.

mod frame;
mod runtime;
mod types;

pub use runtime::spawn_call_session;
pub use types::{
    CallSessionConfiguration, CallSessionConfigurationError, CallSessionDiagnostic,
    CallSessionHandle, CallSessionOutputs, CallSessionTask, IncomingCall, IncomingCallReceiver,
    IncomingCallResponder, OutboundCall, PendingCall, RemoteCallError, SessionCallOutcome,
    SessionSubmitError, TransmissionUncertainReason,
};
