mod connection;
mod sink;
mod stream;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ocpp_client::{TransportSink, TransportStream};
use tokio::sync::{Semaphore, mpsc, watch};

use self::sink::TriggerSink;
use self::stream::TriggerStream;
pub(super) use connection::{connect, connect_201};

use crate::trigger::TriggerJob;
use crate::trigger201::TriggerJob201;
use crate::{Ocpp16State, Ocpp201State};

/// The inbound ID is not passed to typed callbacks. Associate the IDs in the same
/// order the client's single `TriggerMessage` handler consumes its inbound channel.
#[derive(Default)]
struct Pending {
    received: VecDeque<(String, bool)>,
    ready: HashMap<String, ReadyJob>,
    boot_calls: HashMap<String, Instant>,
}
enum ReadyJob {
    V16(TriggerJob),
    V201(TriggerJob201),
}

#[derive(Clone)]
pub(super) struct TriggerBarrier {
    pending: Arc<Mutex<Pending>>,
    capacity: usize,
    permits: Arc<Semaphore>,
    generation: Arc<AtomicU64>,
    state: Option<Arc<Mutex<Ocpp16State>>>,
    timeout: Duration,
    delivered: Option<mpsc::UnboundedSender<TriggerJob>>,
    delivered_201: Option<mpsc::UnboundedSender<TriggerJob201>>,
    state_201: Option<Arc<Mutex<Ocpp201State>>>,
    version: ocpp_client::OcppVersion,
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
            state: Some(state),
            timeout,
            delivered: Some(delivered),
            delivered_201: None,
            state_201: None,
            version: ocpp_client::OcppVersion::V1_6,
            armed,
        }
    }
    pub(super) fn new_201(
        capacity: usize,
        delivered: mpsc::UnboundedSender<TriggerJob201>,
        state: Arc<Mutex<Ocpp201State>>,
        timeout: Duration,
    ) -> Self {
        let (armed, _) = watch::channel(false);
        Self {
            pending: Arc::new(Mutex::new(Pending::default())),
            capacity,
            permits: Arc::new(Semaphore::new(capacity)),
            generation: Arc::new(AtomicU64::new(0)),
            state: None,
            timeout,
            delivered: None,
            delivered_201: Some(delivered),
            state_201: Some(state),
            version: ocpp_client::OcppVersion::V2_0_1,
            armed,
        }
    }

    pub(super) fn arm(&self) {
        self.armed.send_replace(true);
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
        job.generation = self.generation.load(Ordering::SeqCst);
        pending.ready.insert(id, ReadyJob::V16(job));
        true
    }
    pub(super) fn accept_201(&self, mut job: TriggerJob201) -> bool {
        let mut pending = self.pending.lock().expect("trigger barrier lock");
        let Some((id, false)) = pending.received.pop_front() else {
            return false;
        };
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            return false;
        };
        job.permit = Some(permit);
        job.generation = self.generation.load(Ordering::SeqCst);
        pending.ready.insert(id, ReadyJob::V201(job));
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
        if let Some(state) = &self.state_201 {
            state.lock().expect("OCPP 2.0.1 state lock").registered = false;
        } else {
            self.state
                .as_ref()
                .expect("OCPP 1.6 barrier")
                .lock()
                .expect("OCPP 1.6 state lock")
                .registered = false;
        }
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
