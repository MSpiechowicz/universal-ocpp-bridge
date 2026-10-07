use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::oneshot;

use super::{
    ClientDiagnostics, ClientFuture, Command, OcppVersion, ProtocolClient, RemoteCommand,
    RemoteCommandKind, ReplyDelay, ReplyDelayReceipt, SimulatorCall, SimulatorClientError,
    SimulatorProtocolClient, TraceEvent, TraceKind,
};

impl ProtocolClient for SimulatorProtocolClient {
    fn version(&self) -> OcppVersion {
        self.version
    }

    fn accepted_registration(&self) -> Option<bool> {
        self.ocpp16_state
            .as_ref()
            .map(|state| state.lock().expect("OCPP 1.6 state lock").registered)
            .or_else(|| {
                self.ocpp201_state
                    .as_ref()
                    .map(|state| state.lock().expect("OCPP 2.0.1 state lock").registered)
            })
    }
    fn local_authorization(&self) -> Option<crate::local_authorization::LocalAuthorizationHandle> {
        self.ocpp16_state
            .as_ref()
            .and_then(|state| state.lock().expect("OCPP 1.6 state lock").local.clone())
    }
    fn local_authorization201(
        &self,
    ) -> Option<crate::local_authorization201::LocalAuthorization201Handle> {
        self.ocpp201_state
            .as_ref()
            .and_then(|state| state.lock().expect("native state lock").local.clone())
    }
    fn reservation16(&self) -> Option<crate::reservation16::ReservationHandle> {
        self.ocpp16_state.as_ref().and_then(|state| {
            state
                .lock()
                .expect("native state lock")
                .reservation16
                .clone()
        })
    }

    fn firmware16(&self) -> Option<crate::firmware16::FirmwareHandle> {
        self.ocpp16_state
            .as_ref()
            .and_then(|state| state.lock().expect("native state lock").firmware16.clone())
    }

    fn diagnostics16(&self) -> Option<crate::diagnostics16::DiagnosticsHandle> {
        self.ocpp16_state.as_ref().and_then(|state| {
            state
                .lock()
                .expect("native state lock")
                .diagnostics16
                .clone()
        })
    }

    fn firmware201(&self) -> Option<crate::firmware201::Firmware201Handle> {
        self.ocpp201_state
            .as_ref()
            .and_then(|state| state.lock().expect("native state lock").firmware201.clone())
    }

    fn diagnostics201(&self) -> Option<crate::diagnostics201::Diagnostics201Handle> {
        self.ocpp201_state.as_ref().and_then(|state| {
            state
                .lock()
                .expect("native state lock")
                .diagnostics201
                .clone()
        })
    }

    fn reservation201(&self) -> Option<crate::reservation201::Reservation201Handle> {
        self.ocpp201_state.as_ref().and_then(|state| {
            state
                .lock()
                .expect("native state lock")
                .reservation201
                .clone()
        })
    }

    fn reboot_count(&self) -> u64 {
        self.ocpp16_state.as_ref().map_or_else(
            || {
                self.ocpp201_state.as_ref().map_or(0, |state| {
                    state.lock().expect("native state lock").reboot_count
                })
            },
            |state| state.lock().expect("OCPP 1.6 state lock").reboot_count,
        )
    }

    fn socket_connected(&self) -> Option<bool> {
        self.ocpp16_state
            .as_ref()
            .map(|state| state.lock().expect("OCPP 1.6 state lock").socket_connected)
            .or_else(|| {
                self.ocpp201_state
                    .as_ref()
                    .map(|state| state.lock().expect("native state lock").socket_connected)
            })
    }

    fn arm_local_reply_fault(
        &self,
        fault: crate::local_authorization::transport::NativeReplyFault,
    ) -> Result<(), SimulatorClientError> {
        if let Some(state) = &self.ocpp201_state {
            let mut state = state.lock().expect("native state lock");
            if state.local_reply_fault.is_some()
                || matches!(fault, crate::local_authorization::transport::NativeReplyFault::Delay(duration) if duration.is_zero() || duration > Duration::from_secs(30))
            {
                return Err(SimulatorClientError::Protocol(
                    "native reply fault unavailable".to_owned(),
                ));
            }
            state.local_reply_fault = Some(fault);
            return Ok(());
        }
        let state = self
            .ocpp16_state
            .as_ref()
            .ok_or_else(|| SimulatorClientError::Protocol("wrong native edition".to_owned()))?;
        let mut state = state.lock().expect("OCPP 1.6 state lock");
        if state.local_reply_fault.is_some() {
            return Err(SimulatorClientError::Protocol(
                "native reply fault already armed".to_owned(),
            ));
        }
        if matches!(fault, crate::local_authorization::transport::NativeReplyFault::Delay(duration) if duration.is_zero() || duration > Duration::from_secs(30))
        {
            return Err(SimulatorClientError::Protocol(
                "native reply delay bound".to_owned(),
            ));
        }
        state.local_reply_fault = Some(fault);
        Ok(())
    }

    fn heartbeat(&self) -> ClientFuture<'_, String> {
        Box::pin(async move { self.send_command(Command::Heartbeat).await })
    }

    fn call(&self, call: SimulatorCall) -> ClientFuture<'_, serde_json::Value> {
        Box::pin(async move {
            self.send_command(|result| Command::Call(call, result))
                .await
        })
    }

    fn next_remote_command(&self) -> ClientFuture<'_, RemoteCommand> {
        Box::pin(async move { self.send_command(Command::NextRemote).await })
    }

    fn arm_remote_reply_delay(
        &self,
        kind: RemoteCommandKind,
        duration: Duration,
    ) -> Result<ReplyDelayReceipt, SimulatorClientError> {
        if duration.is_zero() || duration > Duration::from_secs(30) {
            return Err(SimulatorClientError::Protocol(
                "invalid remote reply delay".to_owned(),
            ));
        }
        let (receipt, observed) = oneshot::channel();
        let mut slot = self.reply_delay.lock().expect("reply delay lock poisoned");
        if slot.is_some() {
            return Err(SimulatorClientError::Protocol(
                "remote reply delay already armed".to_owned(),
            ));
        }
        *slot = Some(ReplyDelay {
            kind,
            duration,
            receipt,
        });
        Ok(observed)
    }

    fn shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move { self.send_command(Command::Shutdown).await })
    }

    fn force_shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            let response = self.emergency_client.disconnect().await;
            if let Some(state) = &self.ocpp16_state {
                state.lock().expect("OCPP 1.6 state lock").local = None;
                state.lock().expect("native state lock").reservation16 = None;
                state.lock().expect("native state lock").firmware16 = None;
                state.lock().expect("native state lock").diagnostics16 = None;
            }
            if let Some(state) = &self.ocpp201_state {
                let mut state = state.lock().expect("native state lock");
                state.local = None;
                state.reservation201 = None;
                state.firmware201 = None;
                state.diagnostics201 = None;
            }
            response
        })
    }

    fn abort(&self) {
        self.traces
            .push(TraceKind::Stopped, "client task force-stopped");
        self.worker.abort();
        if let Some(state) = &self.ocpp16_state {
            state.lock().expect("OCPP 1.6 state lock").local = None;
            state.lock().expect("native state lock").reservation16 = None;
            state.lock().expect("native state lock").firmware16 = None;
            state.lock().expect("native state lock").diagnostics16 = None;
        }
        if let Some(state) = &self.ocpp201_state {
            let mut state = state.lock().expect("native state lock");
            state.local = None;
            state.reservation201 = None;
            state.firmware201 = None;
            state.diagnostics201 = None;
        }
    }

    fn traces(&self) -> Vec<TraceEvent> {
        self.traces.snapshot()
    }

    fn diagnostics(&self) -> ClientDiagnostics {
        ClientDiagnostics {
            rejected_commands: self.rejected_commands.load(Ordering::Relaxed),
            dropped_traces: self.traces.dropped(),
        }
    }
}
