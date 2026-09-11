use std::{
    convert::Infallible,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::{
    http::header,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures_util::stream;
use serde_json::json;
use tokio::sync::mpsc;
use uob_application::capture::{CaptureLease, TraceWindow};

const SLOW_READER_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

// No queued record bytes or ring references: an unread HTTP body cannot pin the ring.
struct Shared {
    lease: Option<CaptureLease>,
    terminal: &'static str,
}
type SharedLease = Arc<Mutex<Shared>>;

struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Reader {
    shared: SharedLease,
    receiver: mpsc::Receiver<()>,
    _task: Task,
    process: String,
    capture: u64,
    after: Option<u64>,
    initialized: bool,
    drops_reported: u64,
    done: bool,
}

pub(super) fn response(
    lease: CaptureLease,
    process: String,
    capture: u64,
    after: Option<u64>,
) -> Response {
    let shared = Arc::new(Mutex::new(Shared {
        lease: Some(lease),
        terminal: "expiry",
    }));
    let (sender, receiver) = mpsc::channel(1);
    let task = Task(tokio::spawn(notify(
        shared.clone(),
        sender,
        SLOW_READER_TIMEOUT,
    )));
    let reader = Reader {
        shared,
        receiver,
        _task: task,
        process,
        capture,
        after,
        initialized: false,
        drops_reported: 0,
        done: false,
    };
    let output = stream::unfold(reader, |mut reader| async move {
        if reader.done {
            return None;
        }
        loop {
            let _ = reader.receiver.recv().await;
            if let Some(event) = reader.next() {
                return Some((Ok::<Event, Infallible>(event), reader));
            }
        }
    });
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Sse::new(output).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))),
    )
        .into_response()
}

async fn notify(shared: SharedLease, sender: mpsc::Sender<()>, timeout: Duration) {
    let mut progress = Instant::now();
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    loop {
        tokio::select! {
            biased;
            () = sender.closed() => break,
            _ = tick.tick() => {}
        }
        {
            let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
            if state
                .lease
                .as_ref()
                .is_none_or(|lease| !lease.permits(lease.filter()))
            {
                state.terminal = "expiry";
                break;
            }
        }
        match sender.try_send(()) {
            Ok(()) => progress = Instant::now(),
            Err(mpsc::error::TrySendError::Closed(())) => break,
            Err(mpsc::error::TrySendError::Full(())) if progress.elapsed() >= timeout => {
                shared
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .terminal = "slow_reader";
                break;
            }
            Err(mpsc::error::TrySendError::Full(())) => {}
        }
    }
    shared
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .lease
        .take();
}

impl Reader {
    fn next(&mut self) -> Option<Event> {
        let read = {
            let mut state = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(read) = state
                .lease
                .as_ref()
                .and_then(|lease| lease.read_after(self.after).ok())
            {
                read
            } else {
                state.lease.take();
                self.done = true;
                return Some(gap(state.terminal, None));
            }
        };
        let window = read.window;
        if !self.initialized {
            self.initialized = true;
            return Some(
                Event::default().event("trace_window").data(
                    json!({
                        "process_instance_id": self.process, "capture_id": self.capture,
                        "replay": "best_effort", "window": window_value(&window)
                    })
                    .to_string(),
                ),
            );
        }
        if let Some(record) = read.record {
            let missing = self
                .after
                .is_some_and(|v| record.sequence > v.saturating_add(1));
            let requested = self.after.map_or(0, |sequence| sequence.saturating_add(1));
            let overflow = window.evicted_records > 0
                && window.first_sequence.is_some_and(|first| requested < first);
            if overflow || missing {
                self.after = record.sequence.checked_sub(1);
                self.drops_reported = window.dropped_records;
                return Some(gap(
                    if overflow { "overflow" } else { "dropped" },
                    Some(&window),
                ));
            }
            if window.dropped_records > self.drops_reported {
                self.drops_reported = window.dropped_records;
                return Some(gap("dropped", Some(&window)));
            }
            self.after = Some(record.sequence);
            // Only this poll materializes an SSE frame. The queue retains no payload copies.
            let id = serde_json::to_string(&(&self.process, self.capture, record.sequence))
                .expect("trace cursor");
            return Some(Event::default().event("trace").id(id).data(
                std::str::from_utf8(record.diagnostic.encoded_json()).expect("sanitized JSON"),
            ));
        }
        if window.dropped_records > self.drops_reported {
            self.drops_reported = window.dropped_records;
            return Some(gap("dropped", Some(&window)));
        }
        None
    }
}

fn gap(reason: &str, window: Option<&TraceWindow>) -> Event {
    Event::default().event("trace_gap").data(
        json!({
            "reason": reason, "replay": "best_effort", "window": window.map(window_value)
        })
        .to_string(),
    )
}
fn window_value(window: &TraceWindow) -> serde_json::Value {
    json!({
        "first_sequence":window.first_sequence, "next_sequence":window.next_sequence,
        "retained_records":window.retained_records, "retained_bytes":window.retained_bytes,
        "evicted_records":window.evicted_records, "dropped_records":window.dropped_records,
        "shed_records":window.shed_records
    })
}

#[cfg(test)]
mod tests;
