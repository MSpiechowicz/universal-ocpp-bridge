use crate::{Ocpp201State, TraceBuffer, TraceKind};
use std::sync::{Arc, Mutex};

pub(crate) async fn register(
    client: &ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
    state: Arc<Mutex<Ocpp201State>>,
    traces: TraceBuffer,
) {
    client
        .on_reconnect(move |client| {
            let state = Arc::clone(&state);
            let traces = traces.clone();
            async move {
                traces.push(TraceKind::Reconnected, "ocpp2.0.1");
                let (boot, generation, local, firmware) = {
                    let current = state.lock().expect("native state lock");
                    (
                        current.boot.clone(),
                        current.socket_generation,
                        current.local.clone(),
                        current.firmware201.is_some(),
                    )
                };
                // A firmware station re-registers after its firmware reboot.
                if !firmware && !local.is_some_and(|local| local.has_persistence()) {
                    return;
                }
                let Some(boot) = boot else {
                    return;
                };
                let Ok(request) = serde_json::from_value::<
                    ocpp_client::ocpp_types::v201::BootNotificationRequest<
                        ocpp_client::ocpp_types::v201::common::CustomData,
                    >,
                >(boot) else {
                    return;
                };
                // A new transport begins unregistered. Only its correlated native
                // response can enable original offline delivery; no ACK is fabricated.
                let result = crate::local_authorization201::transport::on_generation(
                    generation,
                    None,
                    client.send_boot_notification(request),
                )
                .await;
                if result.is_err() {
                    traces.push(TraceKind::Failed, "native reconnect Boot unavailable");
                }
            }
        })
        .await;
}
