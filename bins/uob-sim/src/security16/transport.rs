//! Native security CALLs: raw `ExtendedTriggerMessage` replies, worker-tick delivery of the
//! durable event outbox and of the messages a trigger asked for.
//!
//! These exchanges use the client's generic `Action` call rather than its typed requests, so the
//! pinned whitepaper schemas, not the typed enums, define what is on the wire.
use super::model::{ExtendedMessage, parse_request, status_name};
use super::{Delivery, SecurityEvent, SecurityHandle, Staged};
use crate::{Command, Ocpp16State, TraceBuffer, TraceKind, TriggerReply};
use ocpp_client::ocpp_1_6::{OCPP1_6Client, OCPP1_6Error};
use ocpp_client::ocpp_types::v16::RpcErrorCode;
use ocpp_client::ocpp_types::v16::TriggerMessageRequest;
use ocpp_client::ocpp_types::v16::common::TriggerMessageRequestRequestedMessage as Requested;
use ocpp_client::{Action, ClientError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use time::OffsetDateTime;

macro_rules! raw_action {
    ($name:ident, $wire:literal) => {
        struct $name;
        impl Action for $name {
            const NAME: &'static str = $wire;
            type Request = Value;
            type Response = Value;
        }
    };
}

raw_action!(SecurityEventNotification, "SecurityEventNotification");
raw_action!(LogStatusNotification, "LogStatusNotification");
raw_action!(
    SignedFirmwareStatusNotification,
    "SignedFirmwareStatusNotification"
);

/// One claimed unit of outbound work for the worker loop.
pub(crate) enum Dispatch {
    Event(SecurityEvent),
    Trigger(Staged),
}

/// Only a station configured with a security model intercepts `ExtendedTriggerMessage`; others
/// keep the client library's `NotImplemented` reply.
pub(crate) fn intercepts(action: &str, state: &Mutex<Ocpp16State>) -> bool {
    action == "ExtendedTriggerMessage"
        && state
            .lock()
            .expect("native state lock")
            .security16
            .is_some()
}

fn handle(state: &Mutex<Ocpp16State>) -> Option<SecurityHandle> {
    state.lock().expect("native state lock").security16.clone()
}

const fn requested(message: ExtendedMessage) -> Option<Requested> {
    match message {
        ExtendedMessage::BootNotification => Some(Requested::BootNotification),
        ExtendedMessage::Heartbeat => Some(Requested::Heartbeat),
        ExtendedMessage::MeterValues => Some(Requested::MeterValues),
        ExtendedMessage::StatusNotification => Some(Requested::StatusNotification),
        _ => None,
    }
}

/// The CALLRESULT payload, or `{"callError": <code>}`. An accepted request stages the messages it
/// asks for; they are released by [`replied`] only after the reply reached the socket.
pub(crate) fn reply(payload: &Value, state: &Arc<Mutex<Ocpp16State>>) -> Value {
    let Some(handle) = handle(state) else {
        return json!({"callError": "NotImplemented"});
    };
    let request = match parse_request(payload) {
        Ok(request) => request,
        Err(code) => return json!({"callError": code}),
    };
    let (configured, omit, delay, connectors) = {
        let model = handle.0.lock();
        let settings = model.settings();
        (
            request.message.reply(&settings.trigger),
            settings.trigger.omit,
            settings.reply_delay,
            model.connectors().to_vec(),
        )
    };
    let (generation, boot_accepted) = {
        let current = state.lock().expect("native state lock");
        (
            current.socket_generation,
            current.boot_accepted_generation == Some(current.socket_generation),
        )
    };
    let mut status = configured;
    let mut messages = Vec::new();
    // Parity with TriggerMessage: a Boot is refused once this connection's Boot was accepted.
    if request.message == ExtendedMessage::BootNotification && boot_accepted {
        status = TriggerReply::Rejected;
    }
    if matches!(status, TriggerReply::Accepted) {
        messages = build(request.message, request.connector_id, &connectors, state);
        // Certificate signing is not modelled: nothing exists to send for it.
        let nothing =
            messages.is_empty() && request.message != ExtendedMessage::SignChargePointCertificate;
        if nothing {
            status = TriggerReply::Rejected;
        }
    }
    let staged = (matches!(status, TriggerReply::Accepted) && !omit && !messages.is_empty())
        .then(|| Staged::new(generation, std::mem::take(&mut messages)));
    handle.0.lock().note_trigger(
        format!("{}:{}", request.message.name(), status_name(status)),
        staged,
        delay,
    );
    json!({"status": status_name(status)})
}

/// Messages for one accepted request, reusing the simulator's native trigger payload builder
/// for the classic classes and its log and firmware models for the whitepaper ones.
fn build(
    message: ExtendedMessage,
    connector_id: Option<i64>,
    connectors: &[u16],
    state: &Arc<Mutex<Ocpp16State>>,
) -> Vec<(ExtendedMessage, Value)> {
    if let Some(requested_message) = requested(message) {
        let request = TriggerMessageRequest {
            connector_id,
            requested_message,
        };
        return crate::trigger::payloads(&request, connectors, state)
            .into_iter()
            .map(|payload| (message, payload))
            .collect();
    }
    // Never hold the OCPP state mutex while inspecting a model mutex.
    let (diagnostics, firmware) = {
        let current = state.lock().expect("native state lock");
        (current.diagnostics16.clone(), current.firmware16.clone())
    };
    let (status, request_id) = match message {
        ExtendedMessage::LogStatusNotification => {
            let (status, request_id) =
                diagnostics.map_or(("Idle", None), |model| model.extended_trigger_status());
            (status.to_owned(), request_id)
        }
        ExtendedMessage::FirmwareStatusNotification => firmware.map_or_else(
            || ("Idle".to_owned(), None),
            |model| model.extended_trigger_status(),
        ),
        _ => return Vec::new(),
    };
    vec![(message, status_payload(&status, request_id))]
}

fn status_payload(status: &str, request_id: Option<i32>) -> Value {
    let mut payload = json!({"status": status});
    if let Some(id) = request_id {
        payload["requestId"] = id.into();
    }
    payload
}

/// A configured reply delay, taken once for the CALL just answered.
pub(crate) fn reply_delay(action: &str, state: &Mutex<Ocpp16State>) -> Option<Duration> {
    if action != "ExtendedTriggerMessage" {
        return None;
    }
    handle(state)?.0.lock().take_reply_delay()
}

/// The reply was written: staged messages may now follow it.
pub(crate) fn replied(action: &str, state: &Mutex<Ocpp16State>) {
    if action == "ExtendedTriggerMessage"
        && let Some(handle) = handle(state)
    {
        handle.0.lock().release_staged();
    }
}

/// Worker tick: claim the next queued event once the connection is usable, and any released
/// trigger of this socket generation.
pub(crate) fn poll(state: &Arc<Mutex<Ocpp16State>>) {
    let (handle, generation, accepted, sender) = {
        let current = state.lock().expect("native state lock");
        let Some(handle) = current.security16.clone() else {
            return;
        };
        let accepted = current.registered
            && current.boot_accepted_generation == Some(current.socket_generation);
        let sender = current
            .notifications
            .as_ref()
            .and_then(tokio::sync::mpsc::WeakSender::upgrade);
        (
            handle,
            current
                .socket_connected
                .then_some(current.socket_generation),
            accepted,
            sender,
        )
    };
    let (Some(generation), Some(sender)) = (generation, sender) else {
        return;
    };
    let now = OffsetDateTime::now_utc();
    if let Some(staged) = handle.0.lock().claim_staged(generation) {
        // Nothing is resent: a trigger that cannot be queued is dropped.
        let _ = sender.try_send(Command::Security16(Dispatch::Trigger(staged), generation));
    }
    if !(accepted || handle.send_before_accepted()) {
        return;
    }
    if let Some(event) = handle.next_event(now)
        && sender
            .try_send(Command::Security16(
                Dispatch::Event(event.clone()),
                generation,
            ))
            .is_err()
    {
        let _ = handle.event_finished(&event, Delivery::Failed, now);
    }
}

/// Deliver one claimed unit of work on the socket generation it was claimed for.
pub(crate) async fn send(
    client: OCPP1_6Client,
    state: Arc<Mutex<Ocpp16State>>,
    traces: TraceBuffer,
    dispatch: Dispatch,
    generation: u64,
) {
    match dispatch {
        Dispatch::Event(event) => send_event(&client, &state, &traces, &event, generation).await,
        Dispatch::Trigger(staged) => {
            for (message, payload) in staged.messages {
                send_message(&client, &state, &traces, message, payload, generation).await;
            }
        }
    }
}

fn delivery<T>(outcome: &Result<T, ClientError<OCPP1_6Error>>) -> Delivery {
    match outcome {
        Ok(_) => Delivery::Acknowledged,
        // A transient server fault is retried; any other CALLERROR is a final answer.
        Err(ClientError::Protocol(error))
            if !matches!(
                error.code,
                RpcErrorCode::InternalError | RpcErrorCode::GenericError
            ) =>
        {
            Delivery::Rejected
        }
        Err(_) => Delivery::Failed,
    }
}

async fn send_event(
    client: &OCPP1_6Client,
    state: &Mutex<Ocpp16State>,
    traces: &TraceBuffer,
    event: &SecurityEvent,
    generation: u64,
) {
    let Some(handle) = handle(state) else {
        return;
    };
    // Delivery before an accepted Boot is a deliberate fault of this station.
    let exchange = crate::client_exchange16::NativeExchange::recovery(
        generation,
        !handle.send_before_accepted(),
    );
    traces.push(TraceKind::ChargingCallSent, "SecurityEventNotification");
    let outcome = exchange
        .run(client.call::<SecurityEventNotification>(event.payload()))
        .await;
    let outcome = delivery(&outcome);
    traces.push(
        if outcome == Delivery::Failed {
            TraceKind::Failed
        } else {
            TraceKind::ChargingCallResult
        },
        "SecurityEventNotification",
    );
    if handle
        .event_finished(event, outcome, OffsetDateTime::now_utc())
        .is_err()
    {
        traces.push(TraceKind::Failed, "security_state_uncertain");
    }
}

async fn send_message(
    client: &OCPP1_6Client,
    state: &Mutex<Ocpp16State>,
    traces: &TraceBuffer,
    message: ExtendedMessage,
    payload: Value,
    generation: u64,
) {
    let exchange = crate::client_exchange16::NativeExchange::recovery(generation, false);
    let name = message.name();
    traces.push(TraceKind::ChargingCallSent, name);
    let sent: Result<bool, ()> = match message {
        ExtendedMessage::LogStatusNotification => exchange
            .run(client.call::<LogStatusNotification>(payload))
            .await
            .map(|_| false)
            .map_err(|_| ()),
        ExtendedMessage::FirmwareStatusNotification => exchange
            .run(client.call::<SignedFirmwareStatusNotification>(payload))
            .await
            .map(|_| false)
            .map_err(|_| ()),
        _ => match requested(message) {
            Some(requested) => exchange
                .run(crate::trigger::send_call(client, &requested, payload))
                .await
                .map_err(|_| ()),
            None => Err(()),
        },
    };
    if sent == Ok(true) {
        // A triggered Boot the CSMS accepted registers this connection, as for TriggerMessage.
        let mut current = state.lock().expect("native state lock");
        if current.socket_generation == generation
            && current.socket_connected
            && current.boot_accepted_generation == Some(generation)
        {
            current.registered = true;
            current.replay_requested = true;
        }
    }
    traces.push(
        if sent.is_ok() {
            TraceKind::ChargingCallResult
        } else {
            TraceKind::Failed
        },
        name,
    );
}
