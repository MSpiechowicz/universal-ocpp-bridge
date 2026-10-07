use super::super::{LocalAuthorization201Handle, device_model, native, validation};
use super::{SharedSink, disconnect};
use crate::Ocpp201State;
use crate::local_authorization::transport::NativeReplyFault;
use ocpp_client::{TransportError, TransportEvent, TransportStream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use zeroize::Zeroize;

pub(super) struct Stream {
    pub(super) inner: Box<dyn TransportStream>,
    pub(super) sink: SharedSink,
    pub(super) state: Arc<Mutex<Ocpp201State>>,
    pub(super) generation: u64,
    pub(super) timeout: std::time::Duration,
    pub(super) replies: VecDeque<(String, [u8; 32], Value)>,
    /// A firmware reboot closes this socket generation like an accepted Reset.
    pub(super) close: Arc<tokio::sync::Notify>,
}
struct PreparedCall {
    local: LocalAuthorization201Handle,
    guard: ReplyGuard,
    reply: Value,
    report: Option<Value>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        disconnect(&self.state, self.generation);
    }
}
impl TransportStream for Stream {
    fn recv<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<TransportEvent>, TransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            loop {
                let received = tokio::select! {
                    biased;
                    () = self.close.notified() => {
                        disconnect(&self.state, self.generation);
                        self.sink.lock().await.inner.close().await?;
                        return Ok(None);
                    }
                    received = self.inner.recv() => received,
                };
                let mut event = match received {
                    Ok(event) => event,
                    Err(error) => {
                        disconnect(&self.state, self.generation);
                        return Err(error);
                    }
                };
                if event.is_none() {
                    disconnect(&self.state, self.generation);
                    return Ok(None);
                }
                let Some(TransportEvent::Frame(text)) = &event else {
                    return Ok(event);
                };
                if text.len() > 64 * 1024 {
                    if let Some(TransportEvent::Frame(text)) = &mut event {
                        text.zeroize();
                    }
                    return Err("native frame capacity".into());
                }
                let Ok(value) = serde_json::from_str::<Value>(text) else {
                    return Ok(event);
                };
                let private = native::PrivateJson(value);
                let Some(frame) = private.0.as_array() else {
                    return Ok(event);
                };
                if matches!(frame.first().and_then(Value::as_u64), Some(3 | 4))
                    && let Some(id) = frame.get(1).and_then(Value::as_str)
                    && self.sink.lock().await.reports.remove(id).is_some()
                {
                    if let Some(TransportEvent::Frame(text)) = &mut event {
                        text.zeroize();
                    }
                    continue;
                }
                if let Some(rejection) = self.sink.lock().await.authorization_reply(frame) {
                    if let Some(TransportEvent::Frame(text)) = &mut event {
                        text.zeroize();
                    }
                    return Ok(Some(TransportEvent::Frame(rejection.to_string())));
                }
                if self.observe_boot(frame).await {
                    return Ok(event);
                }
                if frame.len() != 4 || frame[0] != 2 {
                    return Ok(event);
                }
                let (Some(id), Some(action)) = (frame[1].as_str(), frame[2].as_str()) else {
                    return Ok(event);
                };
                if !matches!(
                    action,
                    "SendLocalList"
                        | "ClearCache"
                        | "GetLocalListVersion"
                        | "GetVariables"
                        | "GetBaseReport"
                        | "GetReport"
                        | "Reset"
                        | "ReserveNow"
                        | "CancelReservation"
                ) && !crate::firmware201::transport::intercepts(action, &self.state)
                {
                    return Ok(event);
                }
                let Some(prepared) = self.prepare_call(id, action, &frame[3], text).await? else {
                    return Ok(event);
                };
                if let Some(TransportEvent::Frame(text)) = &mut event {
                    text.zeroize();
                }
                if self.send_reply(id, action, prepared).await? {
                    return Ok(None);
                }
            }
        })
    }
}
struct ReplyGuard(LocalAuthorization201Handle);
impl Drop for ReplyGuard {
    fn drop(&mut self) {
        self.0.0.lock().reply_pending -= 1;
    }
}
fn native_reply(action: &str, payload: &Value, local: &LocalAuthorization201Handle) -> Value {
    let empty = payload.as_object().is_some_and(|fields| {
        fields.is_empty()
            || (fields.len() == 1
                && fields.get("customData").is_some_and(|custom| {
                    custom["vendorId"]
                        .as_str()
                        .is_some_and(|v| v.chars().count() <= 255)
                }))
    });
    match action {
        "GetVariables" => device_model::reply(payload, local),
        "GetLocalListVersion" if empty => match local.snapshot()["listVersion"].as_i64() {
            Some(version) => json!({"versionNumber":version}),
            None => json!({"callError":"InternalError"}),
        },
        "ClearCache" if empty => {
            json!({"status":if local.clear_cache() {"Accepted"} else {"Rejected"}})
        }
        "SendLocalList" if !local.0.lock().list_enabled => json!({"callError":"NotSupported"}),
        "SendLocalList" => json!({"status":local.update(payload)}),
        "Reset"
            if validation::valid_reset(payload)
                && payload.get("evseId").is_none()
                && local.0.lock().state.active.is_empty() =>
        {
            json!({"status":"Accepted"})
        }
        "Reset" => json!({"status":"Rejected"}),
        _ => json!({"callError":"FormationViolation"}),
    }
}

impl Stream {
    async fn observe_boot(&mut self, frame: &[Value]) -> bool {
        if frame.len() == 3 && frame[0] == 3 {
            if let Some(id) = frame[1].as_str() {
                let mut sink = self.sink.lock().await;
                sink.boots.retain(|_, sent| sent.elapsed() < self.timeout);
                if sink.boots.remove(id).is_some() {
                    let mut state = self.state.lock().expect("native state lock");
                    if state.socket_generation == self.generation {
                        state.registered = serde_json::from_value::<ocpp_client::ocpp_types::v201::BootNotificationResponse<ocpp_client::ocpp_types::v201::common::CustomData>>(frame[2].clone())
                            .is_ok_and(|response| response.status == ocpp_client::ocpp_types::v201::common::RegistrationStatusEnum::Accepted);
                    }
                }
            }
            return true;
        }
        if frame.len() == 5 && frame[0] == 4 {
            if let Some(id) = frame[1].as_str() {
                let mut sink = self.sink.lock().await;
                if sink.boots.remove(id).is_some() {
                    let mut state = self.state.lock().expect("native state lock");
                    if state.socket_generation == self.generation {
                        state.registered = false;
                    }
                }
            }
            return true;
        }
        false
    }

    async fn prepare_call(
        &mut self,
        id: &str,
        action: &str,
        payload: &Value,
        raw: &str,
    ) -> Result<Option<PreparedCall>, TransportError> {
        if id.chars().count() > 36 {
            return Err("native message identity capacity".into());
        }
        let local = self
            .state
            .lock()
            .expect("native state lock")
            .local
            .clone()
            .ok_or("native station stopped")?;
        if action == "Reset" && !local.has_persistence() {
            return Ok(None);
        }
        local.0.lock().reply_pending += 1;
        let guard = ReplyGuard(local.clone());
        let fingerprint: [u8; 32] = Sha256::digest(raw.as_bytes()).into();
        let mut report = None;
        let reply = if let Some((_, previous, response)) =
            self.replies.iter().find(|(previous, _, _)| previous == id)
        {
            if previous == &fingerprint {
                response.clone()
            } else {
                json!({"callError":"FormationViolation"})
            }
        } else {
            let response = if matches!(action, "GetBaseReport" | "GetReport") {
                let mut sink = self.sink.lock().await;
                sink.reports.retain(|_, sent| sent.elapsed() < self.timeout);
                let registered = {
                    let state = self.state.lock().expect("native state lock");
                    state.socket_generation == self.generation && state.registered
                };
                if sink.reports.len() >= 128 || !registered {
                    json!({"status":"Rejected"})
                } else {
                    let (response, notification) = device_model::report(action, payload, &local);
                    report = notification;
                    response
                }
            } else if matches!(action, "ReserveNow" | "CancelReservation") {
                crate::reservation201::transport::reply(action, payload, &self.state)
            } else if action == "UpdateFirmware" {
                crate::firmware201::transport::reply(payload, &self.state)
            } else {
                native_reply(action, payload, &local)
            };
            if self.replies.len() == 128 {
                self.replies.pop_front();
            }
            self.replies
                .push_back((id.to_owned(), fingerprint, response.clone()));
            response
        };
        Ok(Some(PreparedCall {
            local,
            guard,
            reply,
            report,
        }))
    }

    // Returns true only after an actual closed socket. Keep the private reply
    // guard through commit/fault/ACK, and release it before Reset recovery.
    async fn send_reply(
        &mut self,
        id: &str,
        action: &str,
        prepared: PreparedCall,
    ) -> Result<bool, TransportError> {
        let PreparedCall {
            local,
            guard,
            reply,
            report,
        } = prepared;
        let reset = action == "Reset" && reply["status"] == "Accepted";
        let fault = if matches!(
            action,
            "SendLocalList"
                | "ClearCache"
                | "GetLocalListVersion"
                | "ReserveNow"
                | "CancelReservation"
        ) {
            self.state
                .lock()
                .expect("native state lock")
                .local_reply_fault
                .take()
        } else {
            None
        };
        if let Some(NativeReplyFault::Delay(duration)) = fault {
            tokio::time::sleep(duration).await;
        }
        if matches!(fault, Some(NativeReplyFault::DropConnection)) {
            disconnect(&self.state, self.generation);
            self.sink.lock().await.inner.close().await?;
            return Ok(true);
        }
        let response = if let Some(code) = reply.get("callError") {
            json!([4, id, code, "native request unavailable or invalid", {}])
        } else {
            json!([3, id, reply])
        };
        self.sink
            .lock()
            .await
            .inner
            .send(response.to_string())
            .await?;
        if let Some(report) = report {
            let mut sink = self.sink.lock().await;
            let report_id = uuid::Uuid::new_v4().to_string();
            sink.inner
                .send(json!([2, report_id, "NotifyReport", report]).to_string())
                .await?;
            sink.reports.insert(report_id, std::time::Instant::now());
        }
        drop(guard);
        if reset {
            local.reload().map_err(TransportError::from)?;
            self.state.lock().expect("native state lock").reboot_count += 1;
            disconnect(&self.state, self.generation);
            self.sink.lock().await.inner.close().await?;
            return Ok(true);
        }
        Ok(false)
    }
}
