//! Deterministic production-only release-read peer for browser acceptance.
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    task::JoinHandle,
    time::timeout,
};
use uob_management_adapter::{
    ManagementReleaseReadAuthenticator, ManagementReleaseReadConfiguration, release_read_router,
};

const TOKEN: &str = "uob1.production.browser-fixture-release-production-secret";
const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const E: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const F: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
const MAX_REQUEST: usize = 1024;
static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

struct Authenticator;
impl ManagementReleaseReadAuthenticator for Authenticator {
    fn authenticate(&self, token: &str) -> bool {
        token == TOKEN
    }
}

/// Owns the private test socket and its bounded responder task.
pub(crate) struct Fixture {
    socket: PathBuf,
    directory: PathBuf,
    task: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_dir(&self.directory);
    }
}

pub(crate) fn router() -> (axum::Router, Fixture) {
    let directory = std::env::temp_dir().join(format!(
        "uob-browser-fixture-release-{}-{}",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    let socket = directory.join("release-read.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let task = tokio::spawn(serve(listener));
    let router = release_read_router(ManagementReleaseReadConfiguration {
        supervisor_socket: socket.clone(),
        authenticator: std::sync::Arc::new(Authenticator),
    });
    (
        router,
        Fixture {
            socket,
            directory,
            task,
        },
    )
}

async fn serve(listener: UnixListener) {
    while let Ok((stream, _)) = listener.accept().await {
        respond(stream).await;
    }
}

async fn respond(mut stream: UnixStream) {
    let Some(request) = timeout(Duration::from_secs(1), read_request(&mut stream))
        .await
        .ok()
        .flatten()
    else {
        return;
    };
    let encoded = match request {
        Request::Status {} => serde_json::to_vec(&status()),
        Request::Events { after } => serde_json::to_vec(&events(after)),
    };
    let Ok(mut encoded) = encoded else {
        return;
    };
    encoded.push(b'\n');
    let _ = timeout(Duration::from_secs(1), stream.write_all(&encoded)).await;
}

async fn read_request(stream: &mut UnixStream) -> Option<Request> {
    let mut encoded = Vec::with_capacity(128);
    let mut chunk = [0_u8; 256];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        let end = chunk[..read].iter().position(|byte| *byte == b'\n');
        let length = end.unwrap_or(read);
        if encoded.len() + length > MAX_REQUEST || end.is_some_and(|end| end + 1 != read) {
            return None;
        }
        encoded.extend_from_slice(&chunk[..length]);
        if end.is_some() {
            return serde_json::from_slice(&encoded).ok();
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status {},
    Events { after: u64 },
}

fn status() -> serde_json::Value {
    serde_json::json!({
        "protocol": 1,
        "manager_version": "0.1.0",
        "code": "ok",
        "status": {
            "rollback": rollback(),
            "probation": probation(),
            "promotion": promotion(),
            "failures": failures(),
            "sequence": 91,
            "failed_operations": 1,
            "staged_verified_digest": A,
            "last_operation": rollback_record(9),
            "qualification": null
        },
        "activation": {
            "sequence": 42,
            "production": { "digest": B, "phase": "previous-good" },
            "previous_good": B,
            "candidate": { "digest": A, "phase": "quarantined" }
        }
    })
}

fn events(after: u64) -> serde_json::Value {
    let records: Vec<_> = [promotion_record(7), failure_record(8), rollback_record(9)]
        .into_iter()
        .filter(|record| {
            record["sequence"]
                .as_u64()
                .is_some_and(|sequence| sequence > after)
        })
        .collect();
    serde_json::json!({
        "protocol": 1,
        "manager_version": "0.1.0",
        "code": "ok",
        "events": {
            "records": records,
            "oldest_sequence": 7,
            "latest_sequence": 9,
            "truncated": after < 6
        }
    })
}

fn promotion() -> serde_json::Value {
    serde_json::json!({
        "candidate": A,
        "previous": B,
        "configuration_digest": D,
        "production_inputs_digest": E,
        "database_device": 2049,
        "database_inode": 1_048_577,
        "step": "probation",
        "recovery_attempted": false
    })
}

fn probation() -> serde_json::Value {
    serde_json::json!({
        "policy": {
            "required_seconds": 86400,
            "maximum_sample_gap_seconds": 60,
            "profile_digest": E
        },
        "started_unix_seconds": 1_710_000_000,
        "verified_seconds": 240,
        "interrupted_intervals": 0,
        "last": {
            "id": 41,
            "unix_seconds": 1_710_000_240,
            "uptime_seconds": 240,
            "invocation": F,
            "candidate": A,
            "configuration_digest": D,
            "checks": {
                "core_progress": true,
                "storage_progress": true,
                "readiness": true,
                "memory_budget": true,
                "cpu_budget": true,
                "response_latency": true
            }
        }
    })
}

fn failures() -> serde_json::Value {
    serde_json::json!({
        "policy": {
            "startup_seconds": 30,
            "exit_window_seconds": 120,
            "exit_count": 3,
            "readiness_grace_seconds": 30,
            "readiness_interval_seconds": 10,
            "readiness_count": 3
        },
        "decision": "rollback_required",
        "last": observation(),
        "invocation": 1,
        "started_at": 1_710_000_000,
        "core_ready": true,
        "exit_recorded": false,
        "exits": [],
        "readiness_failures": [],
        "staging": "stopped",
        "pending_pressure": null,
        "trigger": failure_audit(),
        "audit": [failure_audit()]
    })
}

fn observation() -> serde_json::Value {
    serde_json::json!({
        "id": 17,
        "at_seconds": 1_710_000_300,
        "signal": { "kind": "watchdog" },
        "resource_pressure": false
    })
}

fn failure_audit() -> serde_json::Value {
    serde_json::json!({
        "observation": observation(),
        "decision": "rollback_required"
    })
}

fn promotion_record(sequence: u64) -> serde_json::Value {
    serde_json::json!({
        "sequence": sequence,
        "uid": 1000,
        "request": { "operation": "promote", "digest": A },
        "result": "ok",
        "actor": "supervisor",
        "decision": {
            "kind": "promote",
            "candidate_digest": A,
            "previous_good_digest": B,
            "evidence_digest": C,
            "configuration_digest": D,
            "compatibility": "accepted",
            "drain": "granted",
            "health": "probation",
            "outcome": "continuing"
        }
    })
}

fn failure_record(sequence: u64) -> serde_json::Value {
    serde_json::json!({
        "sequence": sequence,
        "uid": 0,
        "request": { "operation": "rollback" },
        "result": "ok",
        "actor": "supervisor",
        "decision": {
            "kind": "failure",
            "candidate_digest": A,
            "previous_good_digest": B,
            "observation": observation(),
            "decision": "rollback_required",
            "trigger_id": 17
        }
    })
}

fn rollback() -> serde_json::Value {
    serde_json::json!({
        "trigger_id": 17,
        "quarantined_digest": A,
        "previous_good": B,
        "step": "restored",
        "reason": "eligible_failure"
    })
}

fn rollback_record(sequence: u64) -> serde_json::Value {
    serde_json::json!({
        "sequence": sequence,
        "uid": 0,
        "request": { "operation": "rollback" },
        "result": "ok",
        "actor": "supervisor",
        "decision": {
            "kind": "rollback",
            "quarantined_digest": A,
            "previous_good_digest": B,
            "step": "restored",
            "reason": "eligible_failure"
        }
    })
}
