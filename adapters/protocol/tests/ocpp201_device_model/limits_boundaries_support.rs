use crate::support::*;
use serde_json::{Value, json};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::task::JoinHandle;
use uob_application::*;
use uob_contracts::*;

pub(super) struct Harness {
    pub running: RunningSession,
    pub store: Store,
    pub snapshot: StationSnapshot,
    commands: Arc<ScopedCommandAdmissionPort<Value>>,
    remote: Arc<uob_protocol_adapter::v201::remote_control::RemoteControlSession>,
    database: Database,
    next_id: u32,
}

impl Harness {
    pub fn new() -> impl Future<Output = Self> {
        let database = Database::new();
        let store = database.open();
        Self::connect(database, store, 1, 0)
    }

    fn connect(
        database: Database,
        store: Store,
        generation: u64,
        next_id: u32,
    ) -> impl Future<Output = Self> {
        // One fixture-construction boundary owns the large authenticated setup future.
        Box::pin(async move {
            let running = session("ocpp2.0.1", Duration::from_secs(2)).await;
            let (snapshot, _, remote, coordinator) =
                setup_device(&store, running.handle.clone()).await;
            let (remote, coordinator) = if generation == 1 {
                (remote, coordinator)
            } else {
                drop(coordinator);
                let remote = Arc::try_unwrap(remote)
                    .unwrap_or_else(|_| panic!("unshared fresh remote session"))
                    .with_device_model(Arc::new(store.clone()), generation);
                let remote = Arc::new(remote);
                let coordinator = Arc::new(Coordinator::new(
                    Arc::new(store.clone()),
                    remote.clone(),
                    Arc::new(Clock),
                ));
                (remote, coordinator)
            };
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
                store,
                snapshot,
                commands,
                remote,
                database,
                next_id,
            }
        })
    }

    pub fn reconnect(self) -> impl Future<Output = Self> {
        let Self {
            running,
            store,
            snapshot: _,
            commands,
            remote,
            database,
            next_id,
        } = self;
        remote.detach_device_model();
        drop(commands);
        drop(remote);
        // Retired peer, handle and outputs are not needed across either await.
        let RunningSession { task, server, .. } = running;
        async move {
            task.shutdown(Duration::from_secs(1)).await.unwrap();
            server.abort();
            Self::connect(database, store, 2, next_id).await
        }
    }

    // Every native uniqueId has the same encoded length, including calibration and probes.
    pub fn id(&mut self) -> String {
        let id = format!("boundary-{:06}", self.next_id);
        self.next_id += 1;
        id
    }

    pub fn start(&self, id: &str, action: &str, payload: Value) -> JoinHandle<CommandResult> {
        let mut external = command(&self.snapshot, id, protocol(action, payload));
        external.request.resource = self.snapshot.station.clone();
        let commands = self.commands.clone();
        tokio::spawn(async move { commands.submit(external).await.unwrap() })
    }

    pub async fn call(&mut self, id: &str, action: &str, payload: Value) -> usize {
        let frame = tokio::time::timeout(Duration::from_secs(2), self.running.peer.receive())
            .await
            .expect("native CALL receive bound")
            .expect("native CALL frame")
            .into_text()
            .expect("native text frame");
        let decoded: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(decoded, json!([2, id, action, payload]));
        frame.len()
    }

    pub async fn reply(&mut self, id: &str, payload: Value) {
        self.running
            .peer
            .send_text(json!([3, id, payload]).to_string())
            .await
            .unwrap();
    }

    pub async fn query(&mut self, action: &str, payload: Value) -> (CommandResult, usize) {
        let id = self.id();
        let submitted = self.start(&id, action, payload.clone());
        let bytes = self.call(&id, action, payload.clone()).await;
        let reply = if action == "GetVariables" {
            let results: Vec<Value> = payload["getVariableData"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    json!({
                        "component": entry["component"], "variable": entry["variable"],
                        "attributeStatus": "UnknownVariable",
                    })
                })
                .collect();
            json!({"getVariableResult": results})
        } else {
            json!({"status": "EmptyResultSet"})
        };
        self.reply(&id, reply).await;
        let result = submitted.await.unwrap();
        if action == "GetVariables" {
            accepted(&result);
            let evidence = result.device_model_201.as_ref().unwrap();
            let entries = payload["getVariableData"].as_array().unwrap();
            assert_eq!(evidence.variables.len(), entries.len());
            for (native, requested) in evidence.variables.iter().zip(entries) {
                assert_eq!(native.status, DeviceVariableStatus201::UnknownVariable);
                assert_eq!(
                    native.component.name,
                    requested["component"]["name"].as_str().unwrap()
                );
                assert_eq!(
                    native.variable.name,
                    requested["variable"]["name"].as_str().unwrap()
                );
                assert!(!native.value.present);
            }
        } else {
            assert!(matches!(
                result.lifecycle,
                CommandLifecycle::ProtocolResponse {
                    accepted: false,
                    error: None
                }
            ));
            let evidence = result.device_model_201.as_ref().unwrap();
            assert_eq!(
                evidence.native_ack,
                Some(DeviceReportAck201::EmptyResultSet)
            );
            assert_eq!(evidence.report, DeviceReportState201::NotExpected);
        }
        self.no_extra_call().await;
        (result, bytes)
    }

    pub async fn learn(&mut self, action: &str, name: &str, value: &str) -> CommandResult {
        let id = self.id();
        let entry = json!({
            "component": {"name": "DeviceDataCtrlr"},
            "variable": {"name": name, "instance": action},
        });
        let payload = json!({"getVariableData": [entry]});
        let submitted = self.start(&id, "GetVariables", payload.clone());
        self.call(&id, "GetVariables", payload).await;
        self.reply(
            &id,
            json!({"getVariableResult": [{
                "component": entry["component"], "variable": entry["variable"],
                "attributeStatus": "Accepted", "attributeValue": value,
            }]}),
        )
        .await;
        let result = submitted.await.unwrap();
        accepted(&result);
        self.no_extra_call().await;
        result
    }

    pub async fn deny(&mut self, action: &str, payload: Value) {
        let id = self.id();
        let result = tokio::time::timeout(Duration::from_secs(2), self.start(&id, action, payload))
            .await
            .expect("pre-wire rejection bound")
            .unwrap();
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::Rejected { .. }
        ));
        if let Some(evidence) = result.device_model_201 {
            assert!(evidence.variables.is_empty());
            assert!(evidence.native_ack.is_none());
            assert!(!matches!(
                evidence.report,
                DeviceReportState201::Complete { .. }
            ));
        }
        self.no_extra_call().await;
    }

    pub async fn no_extra_call(&mut self) {
        let barrier = self.id();
        self.running
            .peer
            .send_text(json!([2, barrier, "Heartbeat", {}]).to_string())
            .await
            .unwrap();
        let incoming = tokio::time::timeout(
            Duration::from_secs(2),
            self.running.outputs.incoming.receive(),
        )
        .await
        .expect("heartbeat application handoff bound")
        .unwrap();
        incoming
            .responder
            .respond(&json!({"currentTime": "2026-09-01T02:00:00Z"}))
            .unwrap();
        let reply = receive_json(&mut self.running.peer).await;
        assert_eq!(reply[0], 3);
        assert_eq!(reply[1], barrier);
        assert!(reply[2]["currentTime"].is_string());
        // This is an absence-observation bound, not a sleep used to sequence state changes.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), self.running.peer.receive())
                .await
                .is_err(),
            "unexpected interrogation, split CALL, retry, or replay"
        );
    }

    pub async fn terminal(&self, id: &str) -> CommandResult {
        let request = RequestId::new(id).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let result = self
                    .store
                    .command_result_by_request_id(request.clone())
                    .await
                    .unwrap()
                    .unwrap();
                if !result.device_model_201.as_ref().unwrap().report.pending() {
                    return result;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("durable report terminal state")
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

pub(super) fn entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            json!({
                "component": {"name": "C\\\"é"},
                "variable": {"name": format!("V{index:04}")},
            })
        })
        .collect()
}

pub(super) fn payload(action: &str, count: usize, request: i32) -> Value {
    if action == "GetVariables" {
        json!({"getVariableData": entries(count)})
    } else {
        json!({"requestId": request, "componentVariable": entries(count)})
    }
}
