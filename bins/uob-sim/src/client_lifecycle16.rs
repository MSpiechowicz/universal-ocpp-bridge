use std::sync::{Arc, Mutex};

use crate::{
    ClientFuture, Ocpp16State, OcppVersion, ProtocolClient, RemoteCommand, ReplyDelaySlot,
    SimulatorAction, SimulatorCall, SimulatorClientConfig, SimulatorClientError, TraceBuffer,
    TraceEvent, TraceKind,
};
use tokio::sync::mpsc;

type NativeClient = ocpp_client::ocpp_1_6::OCPP1_6Client;

pub(crate) async fn connect(
    config: &SimulatorClientConfig,
    traces: &TraceBuffer,
    remote_commands: mpsc::Sender<RemoteCommand>,
    state: Arc<Mutex<Ocpp16State>>,
    reply_delay: ReplyDelaySlot,
) -> Result<NativeClient, SimulatorClientError> {
    let (client, barrier, trigger_receiver) = crate::trigger_transport::connect(
        &config.endpoint,
        config.credentials_file.as_deref(),
        config.request_timeout,
        config.reconnect,
        config.command_capacity,
        Arc::clone(&state),
    )
    .await
    .map_err(|_| SimulatorClientError::Connection("native connection unavailable".to_owned()))?;
    crate::client_runtime::register_1_6_handlers(
        &client,
        traces,
        remote_commands,
        config.connectors.clone(),
        Arc::clone(&state),
        reply_delay,
    )
    .await;
    crate::trigger::register(
        &client,
        barrier,
        crate::trigger::TriggerSettings {
            connectors: config.connectors.clone(),
            responses: config.trigger_responses.clone(),
            observation: config.trigger_observation.clone(),
        },
        Arc::clone(&state),
        traces.clone(),
        trigger_receiver,
    )
    .await;
    let reconnect_state = Arc::clone(&state);
    let reconnect_traces = traces.clone();
    let recover_on_reconnect = config.local_authorization.is_some()
        || config.local_authorization_file.is_some()
        || config.reservation16.is_some()
        || config.firmware16.is_some();
    client
        .on_reconnect(move |client| {
            let state = Arc::clone(&reconnect_state);
            let traces = reconnect_traces.clone();
            async move {
                // Reset recovery belongs to the worker, not an automatic socket retry.
                if state
                    .lock()
                    .expect("OCPP 1.6 state lock")
                    .reset_reason
                    .is_some()
                {
                    return;
                }
                let adapter = NativeReplayClient::new(client, Arc::clone(&state), traces.clone());
                if !recover_on_reconnect {
                    traces.push(TraceKind::Reconnected, "ocpp1.6");
                    return;
                }
                if bootstrap(&adapter).await.is_ok() {
                    traces.push(TraceKind::Reconnected, "ocpp1.6");
                } else {
                    traces.push(TraceKind::Failed, "native reconnect recovery failed");
                }
            }
        })
        .await;
    Ok(client)
}

pub(crate) async fn reboot(
    client: &NativeClient,
    config: &SimulatorClientConfig,
    traces: &TraceBuffer,
    remote_commands: mpsc::Sender<RemoteCommand>,
    state: Arc<Mutex<Ocpp16State>>,
    reply_delay: ReplyDelaySlot,
) -> Result<NativeClient, SimulatorClientError> {
    traces.push(TraceKind::ResetReceived, "acknowledged");
    client
        .disconnect()
        .await
        .map_err(|_| private_error("native reset teardown"))?;
    let (local, reset_reason) = {
        let mut state = state.lock().expect("OCPP 1.6 state lock");
        let reset_reason = state
            .reset_reason
            .take()
            .ok_or_else(|| private_error("native reset context unavailable"))?;
        state.registered = false;
        state.active_transactions.clear();
        state.status.clear();
        state.meters.clear();
        state.local_reply_fault = None;
        state.replay_requested = false;
        (
            state
                .local
                .clone()
                .ok_or_else(|| private_error("native station stopped"))?,
            reset_reason,
        )
    };
    local.reload().map_err(private_error)?;
    local.stop_for_reset(&reset_reason).map_err(private_error)?;
    let client = connect(
        config,
        traces,
        remote_commands,
        Arc::clone(&state),
        reply_delay,
    )
    .await?;
    let adapter = NativeReplayClient::new(client.clone(), Arc::clone(&state), traces.clone());
    if let Err(error) = bootstrap(&adapter).await {
        if client.disconnect().await.is_err() {
            traces.push(TraceKind::Failed, "native reset socket teardown failed");
        }
        return Err(error);
    }
    state.lock().expect("OCPP 1.6 state lock").reboot_count += 1;
    traces.push(
        TraceKind::Reconnected,
        if local.has_persistence() {
            "reset_recovered"
        } else {
            "reset_reconnected"
        },
    );
    Ok(client)
}

async fn bootstrap(adapter: &NativeReplayClient) -> Result<(), SimulatorClientError> {
    let boot = adapter.state.lock().expect("OCPP 1.6 state lock").boot.clone().unwrap_or_else(|| {
        serde_json::json!({"chargePointVendor":"UOB Simulator", "chargePointModel":"Local Authorization"})
    });
    let response = adapter
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: boot,
        })
        .await?;
    finish_bootstrap(adapter, response).await
}

async fn finish_bootstrap(
    adapter: &NativeReplayClient,
    response: serde_json::Value,
) -> Result<(), SimulatorClientError> {
    if response.get("status").and_then(serde_json::Value::as_str) != Some("Accepted") {
        return Err(private_error("native recovery registration denied"));
    }
    crate::client_exchange16::NativeExchange::recovery(adapter.generation, true)
        .validate_recovery(&adapter.state)?;
    let local = adapter
        .state
        .lock()
        .expect("OCPP 1.6 state lock")
        .local
        .clone()
        .expect("native local model");
    if local.replay(adapter).await.is_err() {
        adapter
            .traces
            .push(TraceKind::Failed, "offline_replay_uncertain");
    }
    if local
        .snapshot()
        .get("stateAvailable")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(private_error("private_state_unavailable"));
    }
    Ok(())
}

pub(crate) async fn replay_pending(
    client: NativeClient,
    state: Arc<Mutex<Ocpp16State>>,
    traces: TraceBuffer,
    generation: u64,
) -> Result<(), SimulatorClientError> {
    let adapter = NativeReplayClient {
        client,
        state,
        traces,
        generation,
    };
    crate::client_exchange16::NativeExchange::recovery(generation, true)
        .validate_recovery(&adapter.state)?;
    let local = adapter
        .state
        .lock()
        .expect("OCPP 1.6 state lock")
        .local
        .clone()
        .expect("native local model");
    local.replay(&adapter).await
}

struct NativeReplayClient {
    client: NativeClient,
    state: Arc<Mutex<Ocpp16State>>,
    traces: TraceBuffer,
    generation: u64,
}

impl NativeReplayClient {
    fn new(client: NativeClient, state: Arc<Mutex<Ocpp16State>>, traces: TraceBuffer) -> Self {
        let generation = state.lock().expect("OCPP 1.6 state lock").socket_generation;
        Self {
            client,
            state,
            traces,
            generation,
        }
    }

    fn finish_call(
        &self,
        call: &SimulatorCall,
        response: &serde_json::Value,
    ) -> Result<(), SimulatorClientError> {
        crate::client_observation16::observe_local_authorization(
            &self.state,
            call,
            response,
            &self.traces,
        )?;
        crate::client_runtime::update_ocpp16_state(
            &self.state,
            call,
            response,
            crate::client_exchange16::NativeExchange::recovery(self.generation, false),
            false,
        )
    }
}
impl ProtocolClient for NativeReplayClient {
    fn version(&self) -> OcppVersion {
        OcppVersion::V1_6
    }
    fn heartbeat(&self) -> ClientFuture<'_, String> {
        Box::pin(async { Err(private_error("heartbeat unavailable during recovery")) })
    }
    fn call(&self, call: SimulatorCall) -> ClientFuture<'_, serde_json::Value> {
        Box::pin(async move {
            let exchange = crate::client_exchange16::NativeExchange::recovery(
                self.generation,
                call.action != SimulatorAction::BootNotification,
            );
            exchange.validate_recovery(&self.state)?;
            let response = exchange.send(&self.client, &call).await?;
            self.finish_call(&call, &response)?;
            Ok(response)
        })
    }
    fn shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            self.client
                .disconnect()
                .await
                .map_err(|_| private_error("native teardown"))
        })
    }
    fn force_shutdown(&self) -> ClientFuture<'_, ()> {
        self.shutdown()
    }
    fn abort(&self) {
        let client = self.client.clone();
        tokio::spawn(async move {
            let _ = client.disconnect().await;
        });
    }
    fn traces(&self) -> Vec<TraceEvent> {
        Vec::new()
    }
}

fn private_error(code: &'static str) -> SimulatorClientError {
    SimulatorClientError::Protocol(code.to_owned())
}

#[cfg(test)]
#[path = "client_boot_generation_tests.rs"]
mod tests;
