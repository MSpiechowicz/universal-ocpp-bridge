use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ocpp_client::ocpp_types::v16::common::BootNotificationResponseStatus as BootStatus;
use ocpp_client::ocpp_types::v16::{BootNotificationResponse, TriggerMessageRequest};
use ocpp_client::{
    ClientConfig, ReconnectPolicy, Reconnector, TokioExecutor, TokioTimer, TransportError,
    TransportEvent, TransportSink, TransportStream, websocket_transport,
};
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc, watch};

use crate::Ocpp16State;
use crate::trigger::TriggerJob;

/// The inbound ID is not passed to typed callbacks. Associate the IDs in the same
/// order the client's single `TriggerMessage` handler consumes its inbound channel.
#[derive(Default)]
struct Pending {
    received: VecDeque<(String, bool)>,
    ready: HashMap<String, TriggerJob>,
    boot_calls: HashMap<String, Instant>,
}

#[derive(Clone)]
pub(super) struct TriggerBarrier {
    pending: Arc<Mutex<Pending>>,
    capacity: usize,
    permits: Arc<Semaphore>,
    generation: Arc<AtomicU64>,
    state: Arc<Mutex<Ocpp16State>>,
    timeout: Duration,
    delivered: mpsc::UnboundedSender<TriggerJob>,
    armed: watch::Sender<bool>,
}

impl TriggerBarrier {
    pub(super) fn new(
        capacity: usize,
        delivered: mpsc::UnboundedSender<TriggerJob>,
        state: Arc<Mutex<Ocpp16State>>,
        timeout: Duration,
    ) -> Self {
        let (armed, _) = watch::channel(false);
        Self {
            pending: Arc::new(Mutex::new(Pending::default())),
            capacity,
            permits: Arc::new(Semaphore::new(capacity)),
            generation: Arc::new(AtomicU64::new(0)),
            state,
            timeout,
            delivered,
            armed,
        }
    }

    pub(super) fn arm(&self) {
        self.armed.send_replace(true);
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    pub(super) fn generation_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.generation)
    }

    pub(super) fn accept(&self, mut job: TriggerJob) -> bool {
        let mut pending = self.pending.lock().expect("trigger barrier lock");
        let Some((id, false)) = pending.received.pop_front() else {
            return false;
        };
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            return false;
        };
        job.permit = Some(permit);
        job.generation = self.generation();
        pending.ready.insert(id, job);
        true
    }

    pub(super) fn discard_invalid(&self) -> bool {
        let mut pending = self.pending.lock().expect("trigger barrier lock");
        if pending
            .received
            .front()
            .is_some_and(|(_, invalid)| *invalid)
        {
            pending.received.pop_front();
            true
        } else {
            false
        }
    }

    pub(super) fn discard(&self) {
        self.pending
            .lock()
            .expect("trigger barrier lock")
            .received
            .pop_front();
    }

    fn clear(&self) {
        let mut pending = self.pending.lock().expect("trigger barrier lock");
        pending.received.clear();
        pending.ready.clear();
        pending.boot_calls.clear();
    }

    pub(super) fn wrap(
        &self,
        sink: Box<dyn TransportSink>,
        stream: Box<dyn TransportStream>,
    ) -> (Box<dyn TransportSink>, Box<dyn TransportStream>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.lock().expect("OCPP 1.6 state lock").registered = false;
        self.clear();
        (
            Box::new(TriggerSink {
                inner: sink,
                barrier: self.clone(),
                generation,
            }),
            Box::new(TriggerStream {
                inner: stream,
                barrier: self.clone(),
                armed: self.armed.subscribe(),
            }),
        )
    }
}

struct TriggerStream {
    inner: Box<dyn TransportStream>,
    barrier: TriggerBarrier,
    armed: watch::Receiver<bool>,
}

impl TransportStream for TriggerStream {
    fn recv<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<TransportEvent>, TransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            // The read loop starts before typed handlers are registered; hold
            // inbound frames until each captured ID can reach its callback.
            while !*self.armed.borrow_and_update() {
                self.armed
                    .changed()
                    .await
                    .expect("trigger handler registration sender");
            }
            let event = self.inner.recv().await?;
            if let Some(TransportEvent::Frame(text)) = &event {
                if let Ok(Value::Array(frame)) = serde_json::from_str::<Value>(text) {
                    // The client's RawCall parser drops envelopes with any extra element.
                    // Never reserve a FIFO slot for a CALL the typed handler cannot see.
                    if frame.len() == 4
                        && frame.first().and_then(Value::as_u64) == Some(2)
                        && frame.get(2).and_then(Value::as_str) == Some("TriggerMessage")
                        && let Some(payload) = frame.get(3)
                        && serde_json::from_value::<TriggerMessageRequest>(payload.clone()).is_ok()
                        && let Some(id) = frame.get(1).and_then(Value::as_str)
                    {
                        let mut pending =
                            self.barrier.pending.lock().expect("trigger barrier lock");
                        if pending.received.len() + pending.ready.len() >= self.barrier.capacity {
                            return Err("too many pending trigger requests".into());
                        }
                        pending.received.push_back((
                            id.to_owned(),
                            payload.as_object().is_some_and(|fields| {
                                fields
                                    .keys()
                                    .any(|key| key != "requestedMessage" && key != "connectorId")
                            }),
                        ));
                    }
                    let boot_accepted = if let Some(id) = frame.get(1).and_then(Value::as_str)
                        && self
                            .barrier
                            .pending
                            .lock()
                            .expect("trigger barrier lock")
                            .boot_calls
                            .contains_key(id)
                    {
                        match frame.first().and_then(Value::as_u64) {
                            Some(3) if frame.len() == 3 => frame
                                .get(2)
                                .cloned()
                                .and_then(|payload| {
                                    serde_json::from_value::<BootNotificationResponse>(payload).ok()
                                })
                                .map(|response| response.status == BootStatus::Accepted),
                            Some(4)
                                if frame.len() == 5
                                    && frame.get(2).and_then(Value::as_str).is_some()
                                    && frame.get(3).and_then(Value::as_str).is_some() =>
                            {
                                Some(false)
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if let Some(accepted) = boot_accepted
                        && let Some(id) = frame.get(1).and_then(Value::as_str)
                        && self
                            .barrier
                            .pending
                            .lock()
                            .expect("trigger barrier lock")
                            .boot_calls
                            .remove(id)
                            .is_some_and(|sent| sent.elapsed() < self.barrier.timeout)
                    {
                        self.barrier
                            .state
                            .lock()
                            .expect("OCPP 1.6 state lock")
                            .registered = accepted;
                    }
                }
            } else if event.is_none() {
                self.barrier.clear();
            }
            Ok(event)
        })
    }
}

struct TriggerSink {
    inner: Box<dyn TransportSink>,
    barrier: TriggerBarrier,
    generation: u64,
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
                    let _ = self.barrier.delivered.send(job);
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
        self.barrier.clear();
        self.inner.close()
    }
}

struct TriggerReconnector {
    endpoint: String,
    barrier: TriggerBarrier,
}

impl Reconnector for TriggerReconnector {
    fn connect<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        (Box<dyn TransportSink>, Box<dyn TransportStream>),
                        TransportError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let (sink, stream) =
                websocket_transport(&self.endpoint, ocpp_client::OcppVersion::V1_6, None).await?;
            Ok(self.barrier.wrap(sink, stream))
        })
    }
}

pub(super) async fn connect(
    endpoint: &str,
    timeout: std::time::Duration,
    reconnect: bool,
    capacity: usize,
    state: Arc<Mutex<Ocpp16State>>,
) -> Result<
    (
        ocpp_client::ocpp_1_6::OCPP1_6Client,
        TriggerBarrier,
        mpsc::UnboundedReceiver<TriggerJob>,
    ),
    TransportError,
> {
    let (delivered, receiver) = mpsc::unbounded_channel();
    let barrier = TriggerBarrier::new(capacity, delivered, state, timeout);
    let (sink, stream) =
        websocket_transport(endpoint, ocpp_client::OcppVersion::V1_6, None).await?;
    let (sink, stream) = barrier.wrap(sink, stream);
    let mut config = ClientConfig::new(timeout);
    if reconnect {
        config = config.with_reconnect(
            Box::new(TriggerReconnector {
                endpoint: endpoint.to_owned(),
                barrier: barrier.clone(),
            }),
            ReconnectPolicy::default(),
        );
    }
    let client = ocpp_client::Client::from_transport_with_config(
        sink,
        stream,
        Box::new(TokioExecutor),
        Box::new(TokioTimer),
        config,
    );
    Ok((client, barrier, receiver))
}
