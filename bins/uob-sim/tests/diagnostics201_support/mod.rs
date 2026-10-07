//! Shared drivers for the 2.0.1 log station tests: in-memory models, private directories and a
//! loopback HTTP upload receiver.
#![allow(dead_code)]
use axum::{
    Router,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    routing::put,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};
use time::OffsetDateTime;
use tokio::net::TcpListener;
use uob_sim::diagnostics201::{Diagnostics201Config, Diagnostics201Handle, UploadTicket201};

#[must_use]
pub fn config(fields: Value) -> Diagnostics201Config {
    let mut base = json!({"private_state_file": "/unused/diagnostics.json"});
    let Value::Object(fields) = fields else {
        panic!("configuration fields")
    };
    base.as_object_mut().unwrap().extend(fields);
    serde_json::from_value(base).unwrap()
}

#[must_use]
pub fn station(fields: Value) -> Diagnostics201Handle {
    Diagnostics201Handle::in_memory("alpha", &config(fields)).unwrap()
}

/// Deliver every pending status as an acknowledged CALL, returning `(status, requestId)`.
pub fn drain(handle: &Diagnostics201Handle) -> Vec<(String, i32)> {
    let now = OffsetDateTime::now_utc();
    let mut sent = Vec::new();
    while let Some(status) = handle.next_status(now) {
        handle.status_finished(&status, true, now).unwrap();
        sent.push((status.status, status.request_id));
    }
    sent
}

#[must_use]
pub fn names(sent: &[(String, i32)]) -> Vec<&str> {
    sent.iter().map(|(status, _)| status.as_str()).collect()
}

/// Start the next upload attempt.
pub fn attempt(handle: &Diagnostics201Handle) -> UploadTicket201 {
    handle
        .advance(OffsetDateTime::now_utc())
        .unwrap()
        .expect("upload attempt")
}

/// Run one attempt to completion with the given outcome.
pub fn upload(handle: &Diagnostics201Handle, success: bool) -> UploadTicket201 {
    let ticket = attempt(handle);
    handle
        .upload_finished(&ticket, success, OffsetDateTime::now_utc())
        .unwrap();
    ticket
}

#[must_use]
pub fn get_log(log_type: &str, request_id: i64, extra: Value) -> Value {
    let mut payload = json!({
        "logType": log_type,
        "requestId": request_id,
        "log": {"remoteLocation": "http://127.0.0.1:9/uploads/log/"},
    });
    let Value::Object(extra) = extra else {
        panic!("payload fields")
    };
    payload.as_object_mut().unwrap().extend(extra);
    payload
}

/// An owner-only canonical directory, as the durable native models require.
#[must_use]
pub fn private_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("uob-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::canonicalize(directory).unwrap()
}

/// Stored `(slot/file, bytes)` uploads in arrival order.
pub type Files = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// Uploads stored by the loopback receiver, keyed by request path.
#[derive(Clone, Default)]
pub struct Received {
    pub files: Files,
    /// Requests refused with 500 before any is stored.
    pub refuse: Arc<AtomicU32>,
}

impl Received {
    #[must_use]
    pub fn files(&self) -> Vec<(String, Vec<u8>)> {
        self.files.lock().unwrap().clone()
    }
}

async fn store(
    State(received): State<Received>,
    Path((slot, name)): Path<(String, String)>,
    body: Bytes,
) -> StatusCode {
    if received
        .refuse
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
            left.checked_sub(1)
        })
        .is_ok()
    {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    received
        .files
        .lock()
        .unwrap()
        .push((format!("{slot}/{name}"), body.to_vec()));
    StatusCode::CREATED
}

/// A loopback receiver accepting `PUT /uploads/{slot}/{file}`; returns its base URL.
pub async fn receiver(received: Received) -> String {
    let router = Router::new()
        .route("/uploads/{slot}/{name}", put(store))
        .with_state(received);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}
