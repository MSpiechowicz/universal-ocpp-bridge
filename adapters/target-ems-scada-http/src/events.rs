mod payload;
mod recovery;
mod request;
use recovery::recovery;
mod source;
#[cfg(test)]
mod tests;

use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures_util::{StreamExt, stream};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::{
    sync::{Semaphore, mpsc, watch},
    task::JoinHandle,
};
use uob_application::{PageLimit, RetainedEventQuery, TargetQueryPort};

use crate::{error::IntegrationErrorCode as Error, routing::IntegrationState};
use request::{Parameters, Selection};

const MAX_SUBSCRIBERS: usize = 2;
const QUEUE_CAPACITY: u16 = 8;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(2);

/// Honest delivery and replay policy exposed alongside the descriptor.
#[derive(serde::Serialize)]
pub(crate) struct EventPolicy {
    delivery: &'static str,
    durable_replay: &'static str,
    telemetry: &'static str,
    maximum_subscribers: usize,
    queued_records_per_subscriber: u16,
    maximum_payload_bytes: usize,
    slow_subscriber_timeout_seconds: u64,
}

impl Default for EventPolicy {
    fn default() -> Self {
        Self {
            delivery: "local_exposure",
            durable_replay: "within_retention",
            telemetry: "best_effort_not_in_durable_stream",
            maximum_subscribers: MAX_SUBSCRIBERS,
            queued_records_per_subscriber: QUEUE_CAPACITY,
            maximum_payload_bytes: MAX_PAYLOAD_BYTES,
            slow_subscriber_timeout_seconds: DEADLINE.as_secs(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct EventService {
    source: Arc<dyn source::Source>,
    subscribers: Arc<Semaphore>,
    shutdown: watch::Sender<bool>,
}

impl EventService {
    pub(crate) fn new<E: serde::Serialize + Send + 'static>(
        port: Arc<dyn TargetQueryPort<E>>,
    ) -> Self {
        Self {
            source: Arc::new(source::HostSource(port)),
            subscribers: Arc::new(Semaphore::new(MAX_SUBSCRIBERS)),
            shutdown: watch::channel(false).0,
        }
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}

pub(crate) async fn events(
    State(state): State<IntegrationState>,
    headers: HeaderMap,
    query: Result<Query<Parameters>, QueryRejection>,
) -> Response {
    let principal = match state.authenticate(&headers) {
        Ok(Some(principal)) => principal,
        Ok(None) => return Error::Unauthenticated.into_response(),
        Err(error) => return error.into_response(),
    };
    let selection = match query
        .map_err(|_| Error::InvalidRequest)
        .and_then(|Query(query)| query.validate(&headers, principal))
    {
        Ok(selection) => selection,
        Err(error) => return error.into_response(),
    };
    let Some(service) = state.events() else {
        return Error::SourceUnavailable.into_response();
    };
    let Ok(permit) = service.subscribers.clone().try_acquire_owned() else {
        return Error::CapacityExhausted.into_response();
    };
    let stopped = service.shutdown.subscribe();
    if *stopped.borrow() {
        return Error::SourceUnavailable.into_response();
    }
    let open = service.source.open(
        RetainedEventQuery {
            resource: selection.resource.clone(),
            after: selection.after.clone(),
            limit: PageLimit::new(QUEUE_CAPACITY).expect("bounded queue"),
        },
        selection.types.clone(),
    );
    let subscription = match tokio::time::timeout(DEADLINE, open).await {
        Ok(Ok(source)) => source,
        Ok(Err(error)) if error.code() == uob_application::TargetPortErrorCode::CursorExpired => {
            return (StatusCode::GONE, Json(recovery(&selection))).into_response();
        }
        Ok(Err(error)) => return crate::reads::integration_code(error.code()).into_response(),
        Err(_) => return Error::DeadlineExceeded.into_response(),
    };
    let (sender, receiver) = mpsc::channel(usize::from(QUEUE_CAPACITY));
    let task = AbortOnDrop(tokio::spawn(pump(
        subscription,
        sender,
        selection,
        stopped.clone(),
    )));
    // The body owns both the admission slot and the producer task: dropping it cancels pending
    // source reads immediately. Queue count * maximum encoded record bounds retained payloads.
    let output = stream::unfold(
        (receiver, permit, task, service.shutdown.clone()),
        move |(mut receiver, permit, task, lifetime)| {
            let mut stopped = stopped.clone();
            async move {
                let event = tokio::select! {
                    biased;
                    _ = stopped.wait_for(|value| *value) => return None,
                    event = receiver.recv() => event?,
                };
                Some((
                    Ok::<Event, Infallible>(event),
                    (receiver, permit, task, lifetime),
                ))
            }
        },
    );
    Sse::new(output)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

async fn pump(
    mut source: source::Events,
    sender: mpsc::Sender<Event>,
    selection: Selection,
    mut stopped: watch::Receiver<bool>,
) {
    loop {
        let item = tokio::select! {
            biased;
            _ = stopped.wait_for(|value| *value) => return,
            () = sender.closed() => return,
            item = source.next() => item,
        };
        let (event, terminal) = match item {
            Some(Ok(event)) => (event, false),
            Some(Err(Error::CursorExpired)) => (
                Event::default()
                    .event("gap")
                    .data(recovery(&selection).to_string()),
                true,
            ),
            Some(Err(error)) => (
                Event::default()
                    .event("error")
                    .data(serde_json::json!({"error": error.as_str()}).to_string()),
                true,
            ),
            None => return,
        };
        if !matches!(
            tokio::time::timeout(DEADLINE, sender.send(event)).await,
            Ok(Ok(()))
        ) || terminal
        {
            return;
        }
    }
}

struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
