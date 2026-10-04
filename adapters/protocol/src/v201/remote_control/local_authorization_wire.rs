#[cfg(test)]
#[path = "local_authorization_wire_tests.rs"]
mod tests;
use super::local_authorization_values::wipe;
use super::{
    local_authorization_limits::LocalListLimits201,
    local_authorization_values::LocalAuthorizationUpdates201,
};
use serde_json::Value;
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use uob_application::CommandClock;
use uob_contracts::{ResourceRef, SendLocalListReference201, UtcTimestamp};

pub(crate) struct DeferredLocalAuthorizationCall201 {
    pub provider: Option<Arc<LocalAuthorizationUpdates201>>,
    pub clock: Arc<dyn CommandClock>,
    pub resource: ResourceRef,
    pub request: Option<SendLocalListReference201>,
    pub action: &'static str,
    pub authority: Arc<Mutex<bool>>,
    pub(super) limits: Arc<Mutex<LocalListLimits201>>,
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
impl DeferredLocalAuthorizationCall201 {
    fn resolve<T>(
        &self,
        message_id: &str,
        use_update: impl FnOnce(
            &super::local_authorization_values::ProtectedLocalListUpdate201,
            usize,
        ) -> Option<T>,
    ) -> Option<T> {
        let authority = self.authority.lock().ok()?;
        if !*authority {
            return None;
        }
        let active = self.active.lock().ok()?;
        if !*active {
            return None;
        }
        let limits = self.limits.lock().ok()?;
        let now = self.clock.now();
        if now >= self.expires_at {
            return None;
        }
        self.provider.as_ref()?.with_update(
            &self.resource,
            self.request.as_ref()?,
            now,
            |update| {
                if !limits.allows(update.count, update.upsert_count)
                    || (update.has_expiry && limits.supports_expiry == Some(false))
                {
                    return None;
                }
                let mut counter = ByteCounter::default();
                serde_json::to_writer(
                    &mut counter,
                    &(2, message_id, "SendLocalList", update.raw()),
                )
                .ok()?;
                if counter.0 > uob_contracts::LOCAL_AUTHORIZATION_BYTES_LIMIT_201
                    || limits.bytes.is_some_and(|max| counter.0 > max)
                {
                    return None;
                }
                use_update(update, counter.0)
            },
        )?
    }
    pub(crate) fn wire_size(&self, message_id: &str) -> Option<usize> {
        if self.request.is_none() {
            let mut count = ByteCounter::default();
            serde_json::to_writer(
                &mut count,
                &(2, message_id, &self.action, serde_json::json!({})),
            )
            .ok()?;
            return Some(count.0);
        }
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
            if self.request.is_none() {
                let authority = self.authority.lock().ok();
                let active = self.active.lock().ok();
                if authority.as_deref() != Some(&true)
                    || active.as_deref() != Some(&true)
                    || self.clock.now() >= self.expires_at
                {
                    return std::task::Poll::Ready(None);
                }
                let Ok(limits) = self.limits.lock() else {
                    return std::task::Poll::Ready(None);
                };
                if self.action == "ClearCache" && limits.cache_enabled == Some(false) {
                    return std::task::Poll::Ready(None);
                }
                let frame =
                    serde_json::to_string(&(2, message_id, &self.action, serde_json::json!({})))
                        .expect("empty native request");
                send.as_mut().set(Some(factory.take().expect("single send")(
                    axum::extract::ws::Message::Text(frame.into()),
                )));
                return send
                    .as_mut()
                    .as_pin_mut()
                    .expect("initialized send")
                    .poll(cx)
                    .map(Some);
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
