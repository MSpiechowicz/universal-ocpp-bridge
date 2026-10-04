use super::{LocalAuthorization201Handle, transport};
use crate::Ocpp201State;
use serde_json::Value;
use std::sync::{Arc, Mutex};

impl LocalAuthorization201Handle {
    pub(crate) async fn replay_native(
        &self,
        client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
        state: &Arc<Mutex<Ocpp201State>>,
        generation: u64,
    ) -> Result<(), &'static str> {
        self.replay_with(
            |payload: Value| async move {
                let request = serde_json::from_value::<
                    ocpp_client::ocpp_types::v201::TransactionEventRequest<
                        ocpp_client::ocpp_types::v201::common::CustomData,
                    >,
                >(payload.clone());
                let Ok(request) = request else {
                    return false;
                };
                transport::on_generation(
                    generation,
                    Some(payload),
                    client.send_transaction_event(request),
                )
                .await
                .is_ok()
            },
            || {
                let state = state.lock().expect("native state lock");
                state.registered && state.socket_connected && state.socket_generation == generation
            },
        )
        .await
    }
}
