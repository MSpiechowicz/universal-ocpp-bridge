#[cfg(test)]
#[path = "local_authorization16_tests.rs"]
mod tests;
use super::local_authorization_values::{LocalAuthorizationUpdates16, wipe};
use serde_json::Value;
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use uob_application::{CommandClock, CommandDispatchOutcome};
use uob_contracts::{
    Command, CommandOperation, LocalAuthorizationResult16, LocalListUpdateType16, ResourceRef,
    SendLocalListReference16, UtcTimestamp,
};

#[derive(Default)]
pub(super) struct LocalListLimits16 {
    pub enabled: Option<bool>,
    pub send_max: Option<usize>,
    pub list_max: Option<usize>,
}
impl LocalListLimits16 {
    pub fn allows(&self, update_type: LocalListUpdateType16, count: usize) -> bool {
        self.enabled != Some(false)
            && self.send_max.is_none_or(|max| count <= max)
            && (update_type != LocalListUpdateType16::Full
                || self.list_max.is_none_or(|max| count <= max))
    }
    pub fn learn(&mut self, result: &uob_contracts::ConfigurationResult) {
        let uob_contracts::ConfigurationResult::Read {
            keys: Some(keys), ..
        } = result
        else {
            return;
        };
        for key in keys {
            let Some(value) = key.value.as_deref() else {
                continue;
            };
            match key.key.as_str() {
                "LocalAuthListEnabled" => match value {
                    "true" => self.enabled = Some(true),
                    "false" => self.enabled = Some(false),
                    _ => {}
                },
                "SendLocalListMaxLength" => {
                    if let Ok(max) = value.parse::<u32>() {
                        self.send_max = Some(max as usize);
                    }
                }
                "LocalAuthListMaxLength" => {
                    if let Ok(max) = value.parse::<u32>() {
                        self.list_max = Some(max as usize);
                    }
                }
                _ => {}
            }
        }
    }
}

pub(crate) struct DeferredLocalAuthorizationCall16 {
    pub provider: Arc<LocalAuthorizationUpdates16>,
    pub clock: Arc<dyn CommandClock>,
    pub resource: ResourceRef,
    pub request: SendLocalListReference16,
    pub(super) limits: Arc<Mutex<LocalListLimits16>>,
    pub active: Arc<Mutex<bool>>,
    pub expires_at: UtcTimestamp,
}
#[derive(Default)]
struct ByteCounter(usize);
impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl DeferredLocalAuthorizationCall16 {
    fn resolve<T>(
        &self,
        message_id: &str,
        use_update: impl FnOnce(
            &super::local_authorization_values::ProtectedLocalListUpdate16,
            usize,
        ) -> Option<T>,
    ) -> Option<T> {
        let active = self.active.lock().ok()?;
        if !*active {
            return None;
        }
        let limits = self.limits.lock().ok()?;
        let now = self.clock.now();
        if now >= self.expires_at {
            return None;
        }
        self.provider
            .with_update(&self.resource, &self.request, now, |update| {
                if !limits.allows(update.update_type, update.count) {
                    return None;
                }
                let mut counter = ByteCounter::default();
                serde_json::to_writer(
                    &mut counter,
                    &(2, message_id, "SendLocalList", update.raw()),
                )
                .ok()?;
                if counter.0 > uob_contracts::LOCAL_AUTHORIZATION_BYTES_LIMIT_16 {
                    return None;
                }
                use_update(update, counter.0)
            })?
    }
    pub(crate) fn wire_size(&self, message_id: &str) -> Option<usize> {
        self.resolve(message_id, |_, bytes| Some(bytes))
    }
    pub(crate) fn metadata_size(
        &self,
        payload: &Value,
        message_id: &str,
        correlation_id: &str,
    ) -> Option<usize> {
        let mut count = ByteCounter::default();
        serde_json::to_writer(&mut count, payload).ok()?;
        serde_json::to_writer(&mut count, &self.resource).ok()?;
        count
            .0
            .checked_mul(20)?
            .checked_add(message_id.len().checked_mul(4)?)?
            .checked_add(correlation_id.len().checked_mul(4)?)?
            .checked_add(4096)
    }
    pub(crate) async fn send_at_boundary(
        &self,
        connection: &mut crate::StationConnection,
        message_id: &str,
    ) -> Option<Result<(), axum::Error>> {
        self.send_with(message_id, |message| connection.send(message))
            .await
    }
    async fn send_with<F: Future<Output = Result<(), axum::Error>>>(
        &self,
        message_id: &str,
        factory: impl FnOnce(axum::extract::ws::Message) -> F,
    ) -> Option<Result<(), axum::Error>> {
        let mut factory = Some(factory);
        let mut send = std::pin::pin!(None::<F>);
        std::future::poll_fn(|cx| {
            if let Some(future) = send.as_mut().as_pin_mut() {
                return future.poll(cx).map(Some);
            }
            let started = self.resolve(message_id, |update, size| {
                let mut frame = Vec::with_capacity(size);
                if serde_json::to_writer(
                    &mut frame,
                    &(2, message_id, "SendLocalList", update.raw()),
                )
                .is_err()
                {
                    wipe(&mut frame);
                    return None;
                }
                let frame = String::from_utf8(frame).expect("JSON UTF-8");
                send.as_mut().set(Some(factory.take().expect("single send")(
                    axum::extract::ws::Message::Text(frame.into()),
                )));
                Some(
                    send.as_mut()
                        .as_pin_mut()
                        .expect("initialized send")
                        .poll(cx),
                )
            });
            match started {
                Some(poll) => poll.map(Some),
                None => std::task::Poll::Ready(None),
            }
        })
        .await
    }
}

pub(super) fn response(
    action: &str,
    payload: &Value,
    command: &Command<Value>,
) -> CommandDispatchOutcome {
    let uncertain = || super::mapping::uncertain();
    let Some(fields) = payload.as_object() else {
        return uncertain();
    };
    if fields.len() != 1 {
        return uncertain();
    }
    let evidence = match action {
        "GetLocalListVersion" => {
            let Some(version) = payload
                .get("listVersion")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
            else {
                return uncertain();
            };
            LocalAuthorizationResult16::GetLocalListVersion {
                list_version: version,
            }
        }
        "SendLocalList" => {
            let CommandOperation::Ocpp(operation) = &command.operation else {
                return uncertain();
            };
            let Ok(request) =
                serde_json::from_value::<SendLocalListReference16>(operation.payload.clone())
            else {
                return uncertain();
            };
            let Some(status) = payload
                .get("status")
                .and_then(|value| serde::Deserialize::deserialize(value).ok())
            else {
                return uncertain();
            };
            LocalAuthorizationResult16::SendLocalList {
                list_version: request.list_version,
                update_type: request.update_type,
                status,
            }
        }
        "ClearCache" => {
            let Some(status) = payload
                .get("status")
                .and_then(|value| serde::Deserialize::deserialize(value).ok())
            else {
                return uncertain();
            };
            LocalAuthorizationResult16::ClearCache { status }
        }
        _ => return uncertain(),
    };
    CommandDispatchOutcome::LocalAuthorizationResponse16(evidence)
}
