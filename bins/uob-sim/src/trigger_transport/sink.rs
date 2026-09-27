use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::time::Instant;

use ocpp_client::{TransportError, TransportSink};
use serde_json::Value;

use super::{ReadyJob, TriggerBarrier};

pub(super) struct TriggerSink {
    pub(super) inner: Box<dyn TransportSink>,
    pub(super) barrier: TriggerBarrier,
    pub(super) generation: u64,
}

impl TransportSink for TriggerSink {
    fn send<'a>(
        &'a mut self,
        frame: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let parsed = serde_json::from_str::<Value>(&frame).ok();
            let array = parsed.as_ref().and_then(Value::as_array);
            let id = array
                .filter(|array| array.first().and_then(Value::as_u64) == Some(3))
                .and_then(|array| array.get(1).and_then(Value::as_str))
                .map(str::to_owned);
            let boot_id = array
                .filter(|array| {
                    array.first().and_then(Value::as_u64) == Some(2)
                        && array.get(2).and_then(Value::as_str) == Some("BootNotification")
                })
                .and_then(|array| array.get(1).and_then(Value::as_str))
                .map(str::to_owned);
            if let Some(boot_id) = &boot_id {
                let mut pending = self.barrier.pending.lock().expect("trigger barrier lock");
                pending
                    .boot_calls
                    .retain(|_, sent| sent.elapsed() < self.barrier.timeout);
                if pending.boot_calls.len() >= self.barrier.capacity {
                    return Err("too many outstanding BootNotification requests".into());
                }
                pending.boot_calls.insert(boot_id.clone(), Instant::now());
            }
            let result = self.inner.send(frame).await;
            if result.is_err()
                && let Some(boot_id) = &boot_id
            {
                self.barrier
                    .pending
                    .lock()
                    .expect("trigger barrier lock")
                    .boot_calls
                    .remove(boot_id);
            }
            if let Some(id) = id {
                let job = self
                    .barrier
                    .pending
                    .lock()
                    .expect("trigger barrier lock")
                    .ready
                    .remove(&id);
                if result.is_ok()
                    && self.generation == self.barrier.generation.load(Ordering::SeqCst)
                    && let Some(job) = job
                {
                    // The underlying WebSocket write completed before the job becomes visible.
                    match job {
                        ReadyJob::V16(job) => {
                            if let Some(delivered) = &self.barrier.delivered {
                                let _ = delivered.send(job);
                            }
                        }
                        ReadyJob::V201(job) => {
                            if let Some(delivered) = &self.barrier.delivered_201 {
                                let _ = delivered.send(job);
                            }
                        }
                    }
                }
            }
            result
        })
    }

    fn ping<'a>(
        &'a mut self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        self.inner.ping(payload)
    }

    fn pong<'a>(
        &'a mut self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        self.inner.pong(payload)
    }

    fn close<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        self.barrier.generation.fetch_add(1, Ordering::SeqCst);
        self.barrier.clear();
        self.inner.close()
    }
}
