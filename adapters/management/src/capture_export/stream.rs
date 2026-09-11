use std::{
    convert::Infallible,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use futures_util::stream;
use serde_json::json;
use uob_application::capture::{
    CaptureError, CaptureLease, MAX_CAPTURE_EXPORT_DURATION, TraceWindow,
};

use super::format::{MAX_METADATA_BYTES, truncated, window_value};
use crate::ManagementCaptureConfiguration;

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub bytes: usize,
    pub records: usize,
    pub lifetime: Duration,
    pub idle: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 9 * 1024 * 1024,
            records: 2_000,
            lifetime: MAX_CAPTURE_EXPORT_DURATION,
            idle: Duration::from_secs(5),
        }
    }
}

struct Shared {
    configuration: Option<ManagementCaptureConfiguration>,
    credential: HeaderMap,
    manifest: Option<Vec<u8>>,
    capture: u64,
    lease: Option<CaptureLease>,
    started: Instant,
    progress: Instant,
    terminal: Option<&'static str>,
    limits: Limits,
}
impl Shared {
    fn close(&mut self, reason: &'static str) {
        self.terminal.get_or_insert(reason);
        self.lease.take();
        self.manifest.take();
        self.credential.clear();
        self.configuration.take();
    }

    // Revalidate the current credential's entire station/target selection at every body poll
    // and on the independent watchdog, including when the body has never been polled.
    fn check(&mut self) {
        if self.terminal.is_some() {
            return;
        }
        let reason = if self.started.elapsed() >= self.limits.lifetime {
            Some("timeout")
        } else if self.progress.elapsed() >= self.limits.idle {
            Some("slow_reader")
        } else {
            let configuration = self
                .configuration
                .as_ref()
                .expect("active export configuration");
            let grant = self.credential[header::AUTHORIZATION]
                .to_str()
                .ok()
                .and_then(|value| value.strip_prefix("Bearer "))
                .and_then(|token| configuration.authenticator.authenticate(token));
            match grant.map(|grant| configuration.manager.status(&grant)) {
                Some(Ok(status)) if status.id == self.capture => None,
                Some(Err(CaptureError::Gone) | Ok(_)) => Some("capture_stopped_or_expired"),
                _ => Some("permission_revoked"),
            }
        };
        if let Some(reason) = reason {
            self.close(reason);
        }
    }
}

struct Reader {
    shared: Arc<Mutex<Shared>>,
    task: tokio::task::JoinHandle<()>,
    initial: TraceWindow,
    latest: TraceWindow,
    after: Option<u64>,
    records: usize,
    bytes: usize,
    truncated: usize,
    missing_sequences: u64,
    done: bool,
}
impl Drop for Reader {
    fn drop(&mut self) {
        // Do not wait for a task to be rescheduled to release a canceled body's ring lease.
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .close("cancelled");
        self.task.abort();
    }
}

pub(super) fn response(
    configuration: ManagementCaptureConfiguration,
    credential: HeaderMap,
    capture: u64,
    lease: CaptureLease,
    window: TraceWindow,
    manifest: Vec<u8>,
    limits: Limits,
) -> Response {
    let shared = Arc::new(Mutex::new(Shared {
        configuration: Some(configuration),
        credential,
        manifest: Some(manifest),
        capture,
        lease: Some(lease),
        started: Instant::now(),
        progress: Instant::now(),
        terminal: None,
        limits,
    }));
    let watchdog = shared.clone();
    let task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tick.tick().await;
            let mut state = watchdog.lock().unwrap_or_else(PoisonError::into_inner);
            state.check();
            if state.terminal.is_some() {
                break;
            }
        }
    });
    let reader = Reader {
        shared,
        task,
        initial: window,
        latest: window,
        after: window.first_sequence.and_then(|v| v.checked_sub(1)),
        records: 0,
        bytes: 0,
        truncated: 0,
        missing_sequences: 0,
        done: false,
    };
    let output = stream::unfold(reader, |mut reader| async move {
        // Cooperate with charging tasks even when the socket drains without backpressure.
        tokio::task::yield_now().await;
        reader
            .next()
            .map(|bytes| (Ok::<_, Infallible>(bytes), reader))
    });
    (
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"uob-capture-v1.jsonl\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Body::from_stream(output),
    )
        .into_response()
}

impl Reader {
    fn next(&mut self) -> Option<Vec<u8>> {
        if self.done {
            return None;
        }
        let shared = self.shared.clone();
        let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
        state.check();
        state.progress = Instant::now();
        // Never emit previously prepared provenance after permission revocation. A terminal
        // summary without a manifest is deliberately an incomplete file, with no identities.
        if let Some(reason) = state.terminal {
            return Some(self.finish(reason));
        }
        if let Some(manifest) = state.manifest.take() {
            self.bytes += manifest.len();
            return Some(manifest);
        }
        let read = state
            .lease
            .as_ref()
            .and_then(|lease| lease.read_after(self.after).ok());
        let Some(read) = read else {
            state.close("capture_stopped_or_expired");
            return Some(self.finish("capture_stopped_or_expired"));
        };
        self.latest = read.window;
        let record = read
            .record
            .filter(|record| record.sequence < self.initial.next_sequence);
        let Some(record) = record else {
            state.close("window_end");
            return Some(self.finish("window_end"));
        };
        if self.records >= state.limits.records {
            state.close("record_limit");
            return Some(self.finish("record_limit"));
        }
        let bytes = record.diagnostic.encoded_json();
        let prefix = b"{\"type\":\"trace\",\"record\":";
        let size = prefix.len() + bytes.len() + 2;
        if self
            .bytes
            .saturating_add(size)
            .saturating_add(MAX_METADATA_BYTES)
            > state.limits.bytes
        {
            state.close("byte_limit");
            return Some(self.finish("byte_limit"));
        }
        let requested = self
            .after
            .map_or(self.initial.first_sequence.unwrap_or(0), |v| {
                v.saturating_add(1)
            });
        self.missing_sequences = self
            .missing_sequences
            .saturating_add(record.sequence.saturating_sub(requested));
        self.after = Some(record.sequence);
        self.records += 1;
        self.bytes += size;
        self.truncated += usize::from(truncated(bytes));
        let mut chunk = Vec::with_capacity(size);
        chunk.extend_from_slice(prefix);
        chunk.extend_from_slice(bytes);
        chunk.extend_from_slice(b"}\n");
        // No Arc<RetainedTrace> survives this poll. At most one bounded encoded chunk is
        // handed to HTTP; subsequent ring eviction is independent of socket backpressure.
        Some(chunk)
    }

    fn finish(&mut self, reason: &'static str) -> Vec<u8> {
        self.done = true;
        self.task.abort();
        let mut bytes = serde_json::to_vec(&json!({
            "type": "summary", "schema_version": "1.0", "reason": reason,
            "history_complete": false,
            "retained_window_complete": reason == "window_end" && self.records == self.initial.retained_records,
            "exported_records": self.records, "bytes_before_summary": self.bytes,
            "last_sequence": if self.records == 0 { None } else { self.after },
            "unexported_initial_records": self.initial.retained_records.saturating_sub(self.records),
            "missing_sequences": self.missing_sequences, "truncated_records": self.truncated,
            "last_observed_window": window_value(self.latest)
        })).expect("bounded summary scalars");
        bytes.push(b'\n');
        bytes
    }
}
