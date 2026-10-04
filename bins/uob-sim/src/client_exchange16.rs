use std::sync::Mutex;

use crate::{Ocpp16State, SimulatorCall, SimulatorClientError};

tokio::task_local! {
    static EXCHANGE: NativeExchange;
}

/// A native call stays bound to the socket on which its exchange began.
#[derive(Clone, Copy)]
pub(crate) struct NativeExchange {
    pub(crate) generation: u64,
    require_current_boot: bool,
}

impl NativeExchange {
    pub(crate) fn capture(state: &Mutex<Ocpp16State>, call: &SimulatorCall) -> Self {
        let (generation, local) = {
            let state = state.lock().expect("OCPP 1.6 state lock");
            let local = (call.action != crate::SimulatorAction::BootNotification)
                .then(|| state.local.clone())
                .flatten();
            (state.socket_generation, local)
        };
        // Durable models must also fence explicit scenario/API replay calls;
        // ordinary stations without a model retain their existing registration.
        let require_current_boot = local.is_some_and(|handle| handle.has_persistence());
        Self {
            generation,
            require_current_boot,
        }
    }

    pub(crate) fn recovery(generation: u64, require_current_boot: bool) -> Self {
        Self {
            generation,
            require_current_boot,
        }
    }

    pub(crate) fn permits(&self, state: &Ocpp16State, socket_generation: u64) -> bool {
        self.generation == socket_generation
            && self.generation == state.socket_generation
            && state.socket_connected
            && (!self.require_current_boot
                || (state.registered && state.boot_accepted_generation == Some(self.generation)))
    }

    pub(crate) fn validate_recovery(
        &self,
        state: &Mutex<Ocpp16State>,
    ) -> Result<(), SimulatorClientError> {
        let state = state.lock().expect("OCPP 1.6 state lock");
        if self.permits(&state, state.socket_generation) {
            Ok(())
        } else {
            Err(stale_exchange())
        }
    }

    pub(crate) async fn send(
        self,
        client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
        call: &SimulatorCall,
    ) -> Result<serde_json::Value, SimulatorClientError> {
        EXCHANGE
            .scope(self, crate::client_runtime::send_1_6_call(client, call))
            .await
    }
}

pub(crate) fn current() -> Option<NativeExchange> {
    EXCHANGE.try_with(|exchange| *exchange).ok()
}

pub(crate) fn stale_exchange() -> SimulatorClientError {
    SimulatorClientError::Protocol("native exchange generation unavailable".to_owned())
}
