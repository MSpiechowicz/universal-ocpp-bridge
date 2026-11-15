use crate::support::*;
use serde_json::{Value, json};
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle, time::timeout};
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v201::remote_control::RemoteControlSession;

pub const ACTIONS: [&str; 5] = [
    "SetVariableMonitoring",
    "ClearVariableMonitoring",
    "GetMonitoringReport",
    "SetMonitoringBase",
    "SetMonitoringLevel",
];

/// One finished report as the durable store received it.
#[derive(Clone)]
pub struct Finished {
    pub request: RequestId,
    pub evidence: MonitoringResult201,
    pub lifecycle: CommandLifecycle,
}

/// Recording stand-in for the durable report merge. Like storage, it accepts the finished
/// evidence whether or not the application has recorded the acknowledgement yet; it can instead
/// answer "unknown request" or fail.
#[derive(Default)]
pub struct Reports {
    finished: Mutex<Vec<Finished>>,
    attempts: AtomicU32,
    unknown: AtomicBool,
    fail: AtomicBool,
    arrived: Notify,
}

impl Reports {
    pub fn unknown(&self) {
        self.unknown.store(true, Ordering::SeqCst);
    }

    pub fn fail(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }

    pub fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::SeqCst)
    }

    pub fn recorded(&self) -> Vec<(RequestId, MonitoringReportState201)> {
        self.finished
            .lock()
            .unwrap()
            .iter()
            .map(|finished| (finished.request.clone(), report(&finished.evidence)))
            .collect()
    }

    pub fn finished(&self) -> Vec<Finished> {
        self.finished.lock().unwrap().clone()
    }

    /// The first merged report, waiting a bounded time for the collector task.
    pub async fn merged(&self) -> (RequestId, MonitoringReportState201) {
        timeout(Duration::from_secs(10), async {
            loop {
                if let Some(first) = self.recorded().first().cloned() {
                    return first;
                }
                self.arrived.notified().await;
            }
        })
        .await
        .expect("durable report merge bound")
    }
}

fn report(evidence: &MonitoringResult201) -> MonitoringReportState201 {
    let MonitoringOutcome201::Report { report, .. } = &evidence.outcome else {
        panic!("a collector finishes report evidence only");
    };
    report.clone()
}

impl MonitoringReportStore201 for Reports {
    fn finish_monitoring_report(
        &self,
        request: RequestId,
        evidence: MonitoringResult201,
        lifecycle: CommandLifecycle,
        _now: UtcTimestamp,
    ) -> StorageFuture<'_, Option<CommandResult>> {
        Box::pin(async move {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(StorageError::new(StorageErrorCode::Unavailable, "injected"));
            }
            if self.unknown.load(Ordering::SeqCst) {
                return Ok(None);
            }
            self.finished.lock().unwrap().push(Finished {
                request: request.clone(),
                evidence,
                lifecycle,
            });
            self.arrived.notify_one();
            Ok(Some(
                serde_json::from_value(json!({
                    "schema_version": {"major": 1, "revision": 0},
                    "resource": {"bridge_id": "bridge-1", "station_id": "alpha"},
                    "return_route": {"request_id": request.as_str(),
                        "origin": {"kind": "management", "principal_id": "operator"}},
                    "lifecycle": {"stage": "protocol_response", "accepted": true},
                    "recorded_at": "2026-09-01T02:00:00Z"
                }))
                .unwrap(),
            ))
        })
    }
}

pub struct Harness {
    pub running: RunningSession,
    pub snapshot: StationSnapshot,
    pub reports: Arc<Reports>,
    pub remote: Arc<RemoteControlSession>,
    pub store: Store,
    commands: Arc<ScopedCommandAdmissionPort<serde_json::Value>>,
    _database: Database,
    next: u32,
}

impl Harness {
    pub fn new() -> impl Future<Output = Self> {
        Self::build(true)
    }

    /// A session whose host never bound the durable report store.
    pub fn unbound() -> impl Future<Output = Self> {
        Self::build(false)
    }

    fn build(bind_reports: bool) -> impl Future<Output = Self> {
        // One boxed allocation owns the large authenticated setup future.
        Box::pin(async move {
            let running = session("ocpp2.0.1", Duration::from_secs(2)).await;
            let database = Database::new();
            let store = database.open();
            let (mut snapshot, _, remote, coordinator) =
                setup_device(&store, running.handle.clone()).await;
            drop(coordinator);
            for action in ACTIONS {
                snapshot.capabilities.operations.push(SupportedOperation {
                    operation: Operation::ProtocolAction {
                        protocol: ProtocolEdition::Ocpp201,
                        action: action.to_owned(),
                    },
                    parameters: vec![],
                });
            }
            let remote =
                Arc::try_unwrap(remote).unwrap_or_else(|_| panic!("unshared fresh remote session"));
            remote.update_committed(snapshot.clone()).unwrap();
            let reports = Arc::new(Reports::default());
            let remote = if bind_reports {
                remote.with_monitoring_reports(reports.clone())
            } else {
                remote
            };
            let remote = Arc::new(remote);
            let coordinator = Arc::new(Coordinator::new(
                Arc::new(store.clone()),
                remote.clone(),
                Arc::new(Clock),
            ));
            let commands = Arc::new(scoped(
                coordinator,
                &snapshot,
                vec![
                    AccessPermission::Control,
                    AccessPermission::PrivilegedControl,
                ],
            ));
            Self {
                running,
                snapshot,
                reports,
                remote,
                store,
                commands,
                _database: database,
                next: 0,
            }
        })
    }

    pub fn id(&mut self) -> String {
        self.next += 1;
        format!("mon-{:04}", self.next)
    }

    /// Hands one command straight to the session port, as the coordinator does after admission.
    pub fn dispatch(
        &self,
        id: &str,
        action: &str,
        payload: Value,
    ) -> JoinHandle<CommandDispatchOutcome> {
        let mut external = command(&self.snapshot, id, protocol(action, payload));
        external.request.resource = self.snapshot.station.clone();
        let command = external.admit(Clock.now());
        let remote = self.remote.clone();
        tokio::spawn(async move { remote.dispatch(command).await.unwrap() })
    }

    pub async fn call(&mut self, id: &str, action: &str, payload: &Value) {
        let frame = receive_json(&mut self.running.peer).await;
        assert_eq!(frame, json!([2, id, action, payload]));
    }

    pub async fn reply(&mut self, id: &str, payload: Value) {
        self.running
            .peer
            .send_text(json!([3, id, payload]).to_string())
            .await
            .unwrap();
    }

    /// Sends one charger-initiated CALL and returns its native answer.
    pub async fn notify(&mut self, id: &str, action: &str, payload: Value) -> Value {
        self.running
            .peer
            .send_text(json!([2, id, action, payload]).to_string())
            .await
            .unwrap();
        receive_json(&mut self.running.peer).await
    }

    /// Learns a `MonitoringCtrlr` limit through the ordinary read-only device-model path.
    pub async fn learn(&mut self, instance: &str, name: &str, value: &str) {
        self.learn_from("MonitoringCtrlr", instance, name, value)
            .await;
    }

    pub async fn learn_from(&mut self, component: &str, instance: &str, name: &str, value: &str) {
        let id = self.id();
        let entry = json!({
            "component": {"name": component},
            "variable": {"name": name, "instance": instance},
        });
        let payload = json!({"getVariableData": [entry]});
        let mut external = command(
            &self.snapshot,
            &id,
            protocol("GetVariables", payload.clone()),
        );
        external.request.resource = self.snapshot.station.clone();
        let commands = self.commands.clone();
        let submitted = tokio::spawn(async move { commands.submit(external).await.unwrap() });
        self.call(&id, "GetVariables", &payload).await;
        self.reply(
            &id,
            json!({"getVariableResult": [{
                "component": entry["component"], "variable": entry["variable"],
                "attributeStatus": "Accepted", "attributeValue": value,
            }]}),
        )
        .await;
        accepted(&submitted.await.unwrap());
    }

    /// Nothing else was transmitted: a heartbeat barrier is the next thing the peer sees.
    pub async fn no_extra_call(&mut self) {
        let barrier = self.id();
        self.running
            .peer
            .send_text(json!([2, barrier, "Heartbeat", {}]).to_string())
            .await
            .unwrap();
        let incoming = timeout(
            Duration::from_secs(2),
            self.running.outputs.incoming.receive(),
        )
        .await
        .expect("heartbeat handoff bound")
        .unwrap();
        incoming
            .responder
            .respond(&json!({"currentTime": "2026-09-01T02:00:00Z"}))
            .unwrap();
        let reply = receive_json(&mut self.running.peer).await;
        assert_eq!(
            (reply[0].clone(), reply[1].clone()),
            (json!(3), json!(barrier))
        );
        assert!(
            timeout(Duration::from_millis(20), self.running.peer.receive())
                .await
                .is_err(),
            "unexpected transmission"
        );
    }

    pub async fn close(self) {
        self.remote.detach_device_model();
        self.running
            .task
            .shutdown(Duration::from_secs(1))
            .await
            .unwrap();
        self.running.server.abort();
        self.store.shutdown(Duration::from_secs(1)).await.unwrap();
    }
}

pub fn corpus(name: &str) -> Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ocpp-fixtures/corpus/wire/2.0.1")
        .join(format!("monitoring-{name}.json"));
    let frame: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    frame[if frame[0] == 2 { 3 } else { 2 }].clone()
}

/// One exchange: dispatch, observe the exact native CALL, answer it, return the outcome.
pub async fn exchange(
    h: &mut Harness,
    action: &str,
    payload: Value,
    reply: Value,
) -> CommandDispatchOutcome {
    let id = h.id();
    let dispatched = h.dispatch(&id, action, payload.clone());
    h.call(&id, action, &payload).await;
    h.reply(&id, reply).await;
    dispatched.await.unwrap()
}

pub fn evidence(outcome: CommandDispatchOutcome) -> MonitoringResult201 {
    let CommandDispatchOutcome::MonitoringResponse201(result) = outcome else {
        panic!("expected monitoring evidence, got {outcome:?}");
    };
    *result
}

pub fn uncertain(outcome: &CommandDispatchOutcome) -> bool {
    matches!(
        outcome,
        CommandDispatchOutcome::TransmissionUncertain { .. }
    )
}

pub fn not_sent(outcome: &CommandDispatchOutcome, code: CommandErrorCode) -> bool {
    matches!(outcome, CommandDispatchOutcome::NotTransmitted { error } if error.code == code)
}

pub async fn learn_set(h: &mut Harness, items: &str, bytes: &str) {
    h.learn("SetVariableMonitoring", "ItemsPerMessage", items)
        .await;
    h.learn("SetVariableMonitoring", "BytesPerMessage", bytes)
        .await;
}

/// The wire payload of one negative corpus case.
pub fn negative(id: &str) -> Value {
    const CASES: &str = include_str!(
        "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/monitoring-negative-cases.json"
    );
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let case = cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("negative case {id}"));

    case["wire"][3].clone()
}
