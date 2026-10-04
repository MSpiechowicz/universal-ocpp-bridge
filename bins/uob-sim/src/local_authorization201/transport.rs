mod authorization;
mod sink;
mod stream;

use super::{IdToken, native};
use crate::Ocpp201State;
use ocpp_client::{TransportSink, TransportStream};
use serde_json::Value;
use sink::Sink;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use stream::Stream;
use tokio::sync::Mutex as AsyncMutex;

tokio::task_local! { static EXCHANGE_GENERATION: u64; }
tokio::task_local! { static ORIGINAL_EVENT: Option<native::PrivateJson>; }
pub(crate) async fn on_generation<T>(
    generation: u64,
    original: Option<Value>,
    future: impl Future<Output = T>,
) -> T {
    EXCHANGE_GENERATION
        .scope(
            generation,
            ORIGINAL_EVENT.scope(original.map(native::PrivateJson), future),
        )
        .await
}
struct OwnedSink {
    inner: Box<dyn TransportSink>,
    state: Arc<Mutex<Ocpp201State>>,
    generation: u64,
    boots: BTreeMap<String, Instant>,
    authorizations: BTreeMap<String, (IdToken, Instant)>,
    reports: BTreeMap<String, Instant>,
    timeout: Duration,
}
type SharedSink = Arc<AsyncMutex<OwnedSink>>;
pub(crate) fn wrap(
    sink: Box<dyn TransportSink>,
    stream: Box<dyn TransportStream>,
    state: Arc<Mutex<Ocpp201State>>,
    timeout: std::time::Duration,
) -> (Box<dyn TransportSink>, Box<dyn TransportStream>) {
    let generation = {
        let mut current = state.lock().expect("native state lock");
        current.socket_generation += 1;
        current.socket_connected = true;
        current.registered = false;
        current.socket_generation
    };
    let sink = Arc::new(AsyncMutex::new(OwnedSink {
        inner: sink,
        state: Arc::clone(&state),
        generation,
        boots: BTreeMap::new(),
        authorizations: BTreeMap::new(),
        reports: BTreeMap::new(),
        timeout,
    }));
    (
        Box::new(Sink(Arc::clone(&sink))),
        Box::new(Stream {
            inner: stream,
            sink,
            state,
            generation,
            timeout,
            replies: VecDeque::new(),
        }),
    )
}
fn disconnect(state: &Mutex<Ocpp201State>, generation: u64) {
    let mut state = state.lock().expect("native state lock");
    if state.socket_generation == generation {
        state.socket_connected = false;
        state.registered = false;
    }
}
