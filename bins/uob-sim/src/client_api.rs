use crate::{
    OcppVersion, RemoteCommand, RemoteCommandKind, ReplyDelayReceipt, SimulatorCall,
    SimulatorClientError, TraceEvent, local_authorization,
};
use std::pin::Pin;
use std::time::Duration;
pub type ClientFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SimulatorClientError>> + Send + 'a>>;

pub trait ProtocolClient: Send + Sync {
    fn version(&self) -> OcppVersion;
    /// Live OCPP 1.6 registration, including an accepted triggered Boot.
    /// Synthetic scenario clients use the scenario's own scripted state.
    fn accepted_registration(&self) -> Option<bool> {
        None
    }
    fn local_authorization(&self) -> Option<local_authorization::LocalAuthorizationHandle> {
        None
    }
    fn local_authorization201(
        &self,
    ) -> Option<crate::local_authorization201::LocalAuthorization201Handle> {
        None
    }
    fn reservation16(&self) -> Option<crate::reservation16::ReservationHandle> {
        None
    }
    fn reservation201(&self) -> Option<crate::reservation201::Reservation201Handle> {
        None
    }
    fn firmware16(&self) -> Option<crate::firmware16::FirmwareHandle> {
        None
    }
    fn diagnostics16(&self) -> Option<crate::diagnostics16::DiagnosticsHandle> {
        None
    }
    fn firmware201(&self) -> Option<crate::firmware201::Firmware201Handle> {
        None
    }
    fn diagnostics201(&self) -> Option<crate::diagnostics201::Diagnostics201Handle> {
        None
    }
    fn reboot_count(&self) -> u64 {
        0
    }
    /// Observed native socket liveness, independent of registration or handle presence.
    fn socket_connected(&self) -> Option<bool> {
        None
    }
    /// Arm one bounded post-commit native local-list/cache/version reply fault.
    ///
    /// # Errors
    /// Returns an error for unsupported protocol, an occupied fault slot, or a
    /// delay outside the allowed `1..=30_000` millisecond range.
    fn arm_local_reply_fault(
        &self,
        _fault: local_authorization::transport::NativeReplyFault,
    ) -> Result<(), SimulatorClientError> {
        Err(SimulatorClientError::Protocol(
            "native reply fault unsupported".to_owned(),
        ))
    }
    fn heartbeat(&self) -> ClientFuture<'_, String>;
    fn call(&self, _call: SimulatorCall) -> ClientFuture<'_, serde_json::Value> {
        Box::pin(async {
            Err(SimulatorClientError::Protocol(
                "charging calls are unsupported by this client".to_owned(),
            ))
        })
    }
    fn next_remote_command(&self) -> ClientFuture<'_, RemoteCommand> {
        Box::pin(async {
            Err(SimulatorClientError::Protocol(
                "remote commands are unsupported by this client".to_owned(),
            ))
        })
    }
    /// Arms a single matching remote-command reply delay.
    ///
    /// # Errors
    /// Returns an error when the client cannot reserve a reply delay.
    fn arm_remote_reply_delay(
        &self,
        _kind: RemoteCommandKind,
        _duration: Duration,
    ) -> Result<ReplyDelayReceipt, SimulatorClientError> {
        Err(SimulatorClientError::Protocol(
            "remote reply delay is unsupported by this client".to_owned(),
        ))
    }
    fn shutdown(&self) -> ClientFuture<'_, ()>;
    fn force_shutdown(&self) -> ClientFuture<'_, ()>;
    fn abort(&self);
    fn traces(&self) -> Vec<TraceEvent>;
    fn diagnostics(&self) -> ClientDiagnostics {
        ClientDiagnostics::default()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClientDiagnostics {
    pub rejected_commands: u64,
    pub dropped_traces: u64,
}
