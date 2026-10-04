use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ocpp_client::ocpp_types::v16::common::ClearCacheResponseStatus;
use ocpp_client::ocpp_types::v16::{
    ClearCacheResponse, GetLocalListVersionResponse, SendLocalListResponse,
};
use ocpp_client::{TransportError, TransportEvent, TransportSink, TransportStream};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use zeroize::Zeroize;

use super::{LocalAuthorizationHandle, NativeUpdate};
use crate::Ocpp16State;

// A native update may use its entire payload budget; reserve bounded wire
// overhead for the CALL envelope and the accepted <=36-character message ID.
const FRAME_LIMIT: usize = 64 * 1024 + 512;

#[derive(Clone, Copy, Debug)]
pub enum NativeReplyFault {
    Delay(Duration),
    DropConnection,
}

type SharedSink = Arc<AsyncMutex<OwnedSink>>;

struct OwnedSink {
    inner: Box<dyn TransportSink>,
    closed: bool,
    state: Arc<Mutex<Ocpp16State>>,
    generation: u64,
}

impl OwnedSink {
    async fn close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.inner.close().await?;
        self.mark_closed();
        Ok(())
    }

    fn mark_disconnected(&self) {
        let persistent = has_persistent_authorization(&self.state);
        let mut state = self.state.lock().expect("OCPP 1.6 state lock");
        if state.socket_generation == self.generation {
            state.socket_connected = false;
            if persistent || state.reset_reason.is_some() {
                state.registered = false;
            }
        }
    }

    fn mark_closed(&mut self) {
        self.closed = true;
        self.mark_disconnected();
    }
}

fn has_persistent_authorization(state: &Mutex<Ocpp16State>) -> bool {
    // Never hold the OCPP state mutex while inspecting the model mutex.
    let local = state.lock().expect("OCPP 1.6 state lock").local.clone();
    local.is_some_and(|handle| handle.has_persistence())
}

pub(crate) fn wrap(
    sink: Box<dyn TransportSink>,
    stream: Box<dyn TransportStream>,
    state: Arc<Mutex<Ocpp16State>>,
) -> (Box<dyn TransportSink>, Box<dyn TransportStream>) {
    let persistent = has_persistent_authorization(&state);
    let generation = {
        let mut state = state.lock().expect("OCPP 1.6 state lock");
        state.socket_generation = state
            .socket_generation
            .checked_add(1)
            .expect("native socket generation");
        state.socket_connected = true;
        // Ordinary network reconnect retains only the registration already
        // learned from the CSMS. Durable recovery and explicit Reset still
        // require a fresh native Boot; neither path fabricates acceptance.
        if persistent || state.reset_reason.is_some() {
            state.registered = false;
        }
        state.socket_generation
    };
    let sink = Arc::new(AsyncMutex::new(OwnedSink {
        inner: sink,
        closed: false,
        state: Arc::clone(&state),
        generation,
    }));
    (
        Box::new(Sink(Arc::clone(&sink))),
        Box::new(Stream {
            sink,
            inner: stream,
            state,
            replies: VecDeque::new(),
            generation,
        }),
    )
}

struct Sink(SharedSink);
impl TransportSink for Sink {
    fn send<'a>(
        &'a mut self,
        frame: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let Some(exchange) = crate::client_exchange16::current() else {
                return self.0.lock().await.inner.send(frame).await;
            };
            let mut sink = self.0.lock().await;
            let OwnedSink {
                inner,
                state,
                generation,
                ..
            } = &mut *sink;
            let mut send = inner.send(frame);
            std::future::poll_fn(|context| {
                // Validate at the actual transport poll, including the first
                // poll after SDK/sink queuing. The owned transport cannot move
                // buffered bytes to a replacement socket between later polls.
                let current = state.lock().expect("OCPP 1.6 state lock");
                if !exchange.permits(&current, *generation) {
                    return std::task::Poll::Ready(Err(
                        "native exchange generation unavailable".into()
                    ));
                }
                send.as_mut().poll(context)
            })
            .await
        })
    }
    fn ping<'a>(
        &'a mut self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move { self.0.lock().await.inner.ping(payload).await })
    }
    fn pong<'a>(
        &'a mut self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move { self.0.lock().await.inner.pong(payload).await })
    }
    fn close<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move { self.0.lock().await.close().await })
    }
}

struct Stream {
    sink: SharedSink,
    inner: Box<dyn TransportStream>,
    state: Arc<Mutex<Ocpp16State>>,
    replies: VecDeque<(String, [u8; 32], Value)>,
    generation: u64,
}

impl Drop for Stream {
    fn drop(&mut self) {
        let persistent = has_persistent_authorization(&self.state);
        let mut state = self.state.lock().expect("OCPP 1.6 state lock");
        if state.socket_generation == self.generation {
            state.socket_connected = false;
            if persistent || state.reset_reason.is_some() {
                state.registered = false;
            }
        }
    }
}

impl TransportStream for Stream {
    fn recv<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<TransportEvent>, TransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            loop {
                let mut event = match self.inner.recv().await {
                    Ok(event) => event,
                    Err(error) => {
                        self.sink.lock().await.mark_disconnected();
                        return Err(error);
                    }
                };
                if event.is_none() {
                    // Transport EOF is an observed closed connection, not a
                    // second failed close during deliberate Reset/shutdown.
                    self.sink.lock().await.mark_closed();
                    return Ok(None);
                }
                let Some(TransportEvent::Frame(text)) = &event else {
                    return Ok(event);
                };
                if text.len() > FRAME_LIMIT {
                    if let Some(TransportEvent::Frame(text)) = &mut event {
                        text.zeroize();
                    }
                    return Err("native inbound frame capacity".into());
                }
                let Ok(parsed) = serde_json::from_str::<Value>(text) else {
                    return Ok(event);
                };
                let private = PrivateJson(parsed);
                let Some(frame) = private.0.as_array() else {
                    return Ok(event);
                };
                if frame.len() != 4 || frame.first().and_then(Value::as_u64) != Some(2) {
                    return Ok(event);
                }
                let (Some(id), Some(action)) = (frame[1].as_str(), frame[2].as_str()) else {
                    return Ok(event);
                };
                if !matches!(
                    action,
                    "GetLocalListVersion" | "SendLocalList" | "ClearCache" | "Reset"
                ) {
                    return Ok(event);
                }
                let Some(local) = self
                    .state
                    .lock()
                    .expect("OCPP 1.6 state lock")
                    .local
                    .clone()
                else {
                    if let Some(TransportEvent::Frame(text)) = &mut event {
                        text.zeroize();
                    }
                    return Err("native station stopped".into());
                };
                if id.chars().count() > 36 {
                    if let Some(TransportEvent::Frame(text)) = &mut event {
                        text.zeroize();
                    }
                    return Err("native message identity capacity".into());
                }
                let _reply_guard = local.begin_reply();
                let payload = &frame[3];
                let fingerprint: [u8; 32] = Sha256::digest(text.as_bytes()).into();
                let reply = if let Some((_, prior, reply)) =
                    self.replies.iter().find(|(prior_id, _, _)| prior_id == id)
                {
                    if *prior == fingerprint {
                        reply.clone()
                    } else {
                        serde_json::json!({"callError":true})
                    }
                } else {
                    let reply = native_reply(action, payload, &local, text.len());
                    if self.replies.len() == 128 {
                        self.replies.pop_front();
                    }
                    self.replies
                        .push_back((id.to_owned(), fingerprint, reply.clone()));
                    reply
                };
                if let Some(TransportEvent::Frame(text)) = &mut event {
                    text.zeroize();
                }
                if self.finish_reply(id, action, payload, &reply).await? {
                    return Ok(None);
                }
            }
        })
    }
}

impl Stream {
    async fn finish_reply(
        &mut self,
        id: &str,
        action: &str,
        payload: &Value,
        reply: &Value,
    ) -> Result<bool, TransportError> {
        let successful_reset =
            action == "Reset" && reply.get("status").and_then(Value::as_str) == Some("Accepted");
        let fault = if action == "Reset" {
            None
        } else {
            self.state
                .lock()
                .expect("OCPP 1.6 state lock")
                .local_reply_fault
                .take()
        };
        if let Some(NativeReplyFault::Delay(duration)) = fault {
            tokio::time::sleep(duration).await;
        }
        if matches!(fault, Some(NativeReplyFault::DropConnection)) {
            self.sink.lock().await.close().await?;
            return Ok(true);
        }
        let response = if reply.get("callError").is_some() {
            let code = reply
                .get("callError")
                .and_then(Value::as_str)
                .unwrap_or("FormationViolation");
            serde_json::json!([
                4,
                id,
                code,
                "native local authorization request unavailable or invalid",
                {}
            ])
        } else {
            serde_json::json!([3, id, reply])
        };
        self.sink
            .lock()
            .await
            .inner
            .send(response.to_string())
            .await?;
        if successful_reset {
            {
                let mut state = self.state.lock().expect("OCPP 1.6 state lock");
                state.reset_reason = Some(match payload.get("type").and_then(Value::as_str) {
                    Some("Hard") => ocpp_client::ocpp_types::v16::common::Reason::HardReset,
                    Some("Soft") => ocpp_client::ocpp_types::v16::common::Reason::SoftReset,
                    _ => unreachable!("validated native reset"),
                });
            }
            self.sink.lock().await.close().await?;
            return Ok(true);
        }
        Ok(false)
    }
}

fn native_reply(
    action: &str,
    payload: &Value,
    local: &LocalAuthorizationHandle,
    wire_bytes: usize,
) -> Value {
    match action {
        "GetLocalListVersion" if payload.as_object().is_some_and(serde_json::Map::is_empty) => {
            let Some(version) = local.snapshot()["listVersion"].as_i64() else {
                return serde_json::json!({"callError":"InternalError"});
            };
            serde_json::to_value(GetLocalListVersionResponse {
                list_version: version,
            })
            .expect("native version serialization")
        }
        "ClearCache" if payload.as_object().is_some_and(serde_json::Map::is_empty) => {
            serde_json::to_value(ClearCacheResponse {
                status: if local.clear_cache() {
                    ClearCacheResponseStatus::Accepted
                } else {
                    ClearCacheResponseStatus::Rejected
                },
            })
            .expect("native cache serialization")
        }
        "SendLocalList" if wire_bytes <= FRAME_LIMIT => match NativeUpdate::deserialize(payload) {
            Ok(update) => serde_json::to_value(SendLocalListResponse {
                status: local.update(&update),
            })
            .expect("native list serialization"),
            Err(_) => serde_json::json!({"callError":true}),
        },
        "Reset"
            if payload.as_object().is_some_and(|fields| fields.len() == 1)
                && payload
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "Hard" | "Soft")) =>
        {
            serde_json::json!({"status":"Accepted"})
        }
        _ => serde_json::json!({"callError":true}),
    }
}

struct PrivateJson(Value);
impl Drop for PrivateJson {
    fn drop(&mut self) {
        wipe_json(&mut self.0);
    }
}

fn wipe_json(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(wipe_json),
        Value::Object(fields) => fields.values_mut().for_each(wipe_json),
        _ => {}
    }
}
