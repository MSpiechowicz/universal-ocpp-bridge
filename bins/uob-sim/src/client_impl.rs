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
        Box::pin(async move { self.emergency_client.disconnect().await })
    }

    fn abort(&self) {
        self.traces
            .push(TraceKind::Stopped, "client task force-stopped");
        self.worker.abort();
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
