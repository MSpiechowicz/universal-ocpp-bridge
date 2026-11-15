//! Native monitoring replies, worker-tick progression and ordered `NotifyEvent` delivery on the
//! socket generation that accepted the Boot.
use super::{
    DeviceVariable201, EventMessage201, EventOutcome201, MONITORING_ACTIONS_201,
    Monitoring201Handle, Reply201,
};
use crate::{Ocpp201State, TraceBuffer, TraceKind};
use ocpp_client::ClientError;
use ocpp_client::ocpp_2_0_1::OCPP2_0_1Client;
use ocpp_client::ocpp_types::v201::NotifyEventRequest;
use ocpp_client::ocpp_types::v201::common::CustomData;
use serde_json::json;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use tokio::task::JoinSet;

pub(crate) fn handle(state: &Mutex<Ocpp201State>) -> Option<Monitoring201Handle> {
    state
        .lock()
        .expect("native state lock")
        .monitoring201
        .clone()
}

/// Only a station configured with a monitoring model intercepts these CALLs; others keep the
/// client library's `NotImplemented` reply.
pub(crate) fn intercepts(action: &str, state: &Mutex<Ocpp201State>) -> bool {
    MONITORING_ACTIONS_201.contains(&action) && handle(state).is_some()
}

pub(crate) fn reply(
    action: &str,
    payload: &serde_json::Value,
    state: &Mutex<Ocpp201State>,
) -> Reply201 {
    handle(state).map_or_else(
        || Reply201 {
            payload: json!({"callError": "NotImplemented"}),
            report: None,
        },
        |handle| handle.handle_call(action, payload),
    )
}

/// The monitoring part of the device model; empty for a station without one.
pub(crate) fn device_variables(state: &Mutex<Ocpp201State>) -> Vec<DeviceVariable201> {
    // Never hold the station state mutex while inspecting the monitoring model mutex.
    handle(state).map_or_else(Vec::new, |handle| handle.device_variables())
}

/// Worker tick: raise due periodic events and send one `NotifyEvent`.
pub(crate) fn tick(
    client: &OCPP2_0_1Client,
    state: &Arc<Mutex<Ocpp201State>>,
    tasks: &mut JoinSet<()>,
    traces: &TraceBuffer,
) {
    let (handle, generation, registered) = {
        let current = state.lock().expect("native state lock");
        let Some(handle) = current.monitoring201.clone() else {
            return;
        };
        (
            handle,
            current.socket_generation,
            current.registered && current.socket_connected,
        )
    };
    let now = OffsetDateTime::now_utc();
    handle.advance(now);
    if !registered || !tasks.is_empty() {
        return;
    }
    if let Some(message) = handle.next_event(now) {
        tasks.spawn(send(
            client.clone(),
            handle,
            traces.clone(),
            message,
            generation,
        ));
    }
}

async fn send(
    client: OCPP2_0_1Client,
    handle: Monitoring201Handle,
    traces: TraceBuffer,
    message: EventMessage201,
    generation: u64,
) {
    let request: Result<NotifyEventRequest<CustomData>, _> =
        serde_json::from_value(message.to_request());
    let Ok(request) = request else {
        // A message the typed client cannot encode would wedge the queue.
        handle.event_finished(
            &message,
            EventOutcome201::CallError,
            OffsetDateTime::now_utc(),
        );
        return;
    };
    traces.push(TraceKind::ChargingCallSent, "NotifyEvent");
    let result = crate::local_authorization201::transport::on_generation(
        generation,
        None,
        client.send_notify_event(request),
    )
    .await;
    let outcome = match &result {
        Ok(_) => EventOutcome201::Acknowledged,
        // A CALLERROR or undecodable CALLRESULT was still received by the CSMS: never resend it.
        Err(ClientError::Protocol(_) | ClientError::Decode(_)) => EventOutcome201::CallError,
        Err(_) => EventOutcome201::Failed,
    };
    traces.push(
        if outcome == EventOutcome201::Acknowledged {
            TraceKind::ChargingCallResult
        } else {
            TraceKind::Failed
        },
        "NotifyEvent",
    );
    handle.event_finished(&message, outcome, OffsetDateTime::now_utc());
}
