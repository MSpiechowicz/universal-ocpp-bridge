#![allow(clippy::result_large_err)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use uob_sim::scenario::{
    ConnectFuture, ScenarioConnector, ScenarioRunner, cancellation_pair, parse_configuration,
    parse_scenario,
};
use uob_sim::{
    ClientFuture, OcppVersion, ProtocolClient, RemoteCommand, RemoteCommandKind, SimulatorAction,
    SimulatorCall, SimulatorClientConfig,
};

const CONFIG_16: &str = r#"
schema_version = 1
[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/alpha"
ocpp_version = "1.6"
connectors = [1, 2]
request_timeout_ms = 100
command_capacity = 8
trace_capacity = 16
"#;

const CONFIG_201: &str = r#"
schema_version = 1
[[stations]]
id = "alpha"
endpoint = "ws://127.0.0.1:9000/alpha"
ocpp_version = "2.0.1"
request_timeout_ms = 100
command_capacity = 8
trace_capacity = 16
[[stations.evses]]
id = 1
connectors = [1]
[[stations.evses]]
id = 2
connectors = [1]
"#;

#[derive(Default)]
struct PeerEvidence {
    calls: Mutex<Vec<SimulatorAction>>,
    remote_pulls: AtomicUsize,
}

struct FakeConnector {
    version: OcppVersion,
    evidence: Arc<PeerEvidence>,
}

impl ScenarioConnector for FakeConnector {
    fn connect(&self, _configuration: SimulatorClientConfig) -> ConnectFuture<'_> {
        let client = FakeClient {
            version: self.version,
            evidence: Arc::clone(&self.evidence),
            remotes: Mutex::new(VecDeque::from([remote(self.version)])),
        };
        Box::pin(async move { Ok(Box::new(client) as Box<dyn ProtocolClient>) })
    }
}

struct FakeClient {
    version: OcppVersion,
    evidence: Arc<PeerEvidence>,
    remotes: Mutex<VecDeque<RemoteCommand>>,
}

impl ProtocolClient for FakeClient {
    fn version(&self) -> OcppVersion {
        self.version
    }

    fn heartbeat(&self) -> ClientFuture<'_, String> {
        Box::pin(async { Ok("2026-09-01T00:00:00Z".to_owned()) })
    }

    fn call(&self, call: SimulatorCall) -> ClientFuture<'_, serde_json::Value> {
        self.evidence.calls.lock().unwrap().push(call.action);
        let version = self.version;
        Box::pin(async move { Ok(response(version, &call)) })
    }

    fn next_remote_command(&self) -> ClientFuture<'_, RemoteCommand> {
        self.evidence.remote_pulls.fetch_add(1, Ordering::SeqCst);
        let command = self.remotes.lock().unwrap().pop_front();
        Box::pin(async move {
            command.ok_or_else(|| uob_sim::SimulatorClientError::Protocol("no command".to_owned()))
        })
    }

    fn shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn force_shutdown(&self) -> ClientFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn abort(&self) {}

    fn traces(&self) -> Vec<uob_sim::TraceEvent> {
        Vec::new()
    }
}

fn response(version: OcppVersion, call: &SimulatorCall) -> serde_json::Value {
    match call.action {
        SimulatorAction::BootNotification => serde_json::json!({
            "currentTime": "2026-09-01T00:00:00Z", "interval": 60, "status": "Accepted"
        }),
        SimulatorAction::Authorize => {
            let token = match version {
                OcppVersion::V1_6 => call.payload["idTag"].as_str(),
                OcppVersion::V2_0_1 => call
                    .payload
                    .pointer("/idToken/idToken")
                    .and_then(|v| v.as_str()),
            };
            let status = match token {
                Some("DENIED") => "Invalid",
                Some("EXPIRED") => "Expired",
                _ => "Accepted",
            };
            match version {
                OcppVersion::V1_6 => serde_json::json!({"idTagInfo": {"status": status}}),
                OcppVersion::V2_0_1 => serde_json::json!({"idTokenInfo": {"status": status}}),
            }
        }
        SimulatorAction::StartTransaction if version == OcppVersion::V1_6 => serde_json::json!({
            "idTagInfo": {"status": "Accepted"}, "transactionId": 42
        }),
        SimulatorAction::StartTransaction => {
            serde_json::json!({"idTokenInfo": {"status": "Accepted"}})
        }
        SimulatorAction::StatusNotification
        | SimulatorAction::MeterValues
        | SimulatorAction::StopTransaction => serde_json::json!({}),
    }
}

fn remote(version: OcppVersion) -> RemoteCommand {
    let payload = match version {
        OcppVersion::V1_6 => serde_json::json!({"connectorId": 2, "idTag": "REMOTE-USER"}),
        OcppVersion::V2_0_1 => serde_json::json!({
            "evseId": 2,
            "remoteStartId": 7,
            "idToken": {"idToken": "REMOTE-USER", "type": "Central"}
        }),
    };
    RemoteCommand {
        kind: RemoteCommandKind::StartTransaction,
        payload,
        accepted: true,
    }
}

async fn run(
    configuration: &str,
    scenario: &str,
    version: OcppVersion,
) -> (uob_sim::scenario::RunReport, Arc<PeerEvidence>) {
    let configuration = parse_configuration(configuration).unwrap();
    let scenario = parse_scenario(scenario).unwrap();
    let evidence = Arc::new(PeerEvidence::default());
    let runner = ScenarioRunner::new(
        Arc::new(FakeConnector {
            version,
            evidence: Arc::clone(&evidence),
        }),
        Arc::new(uob_sim::scenario::RealClock),
    );
    let report = runner
        .run(
            &configuration,
            &scenario,
            scenario.seed,
            cancellation_pair().1,
        )
        .await;
    (report, evidence)
}

#[tokio::test]
async fn both_editions_cover_offline_authorization_expiry_deduplication_and_uncertainty() {
    for (configuration, scenario, version) in [
        (
            CONFIG_16,
            include_str!("../examples/resilience-1.6.toml"),
            OcppVersion::V1_6,
        ),
        (
            CONFIG_201,
            include_str!("../examples/resilience-2.0.1.toml"),
            OcppVersion::V2_0_1,
        ),
    ] {
        let (report, evidence) = run(configuration, scenario, version).await;
        assert!(report.failure.is_none(), "{:?}", report.failure);
        for detail in [
            "expected_failure:missing_id_tag",
            "expected_failure:missing_id_token",
            "expected_failure:not_authorized",
            "expected_failure:command_expired",
            "expected_failure:transmission_uncertain",
            "duplicate_suppressed",
            "confirmed_without_replay:physical_effects=2",
        ] {
            if detail.contains("id_tag") && version == OcppVersion::V2_0_1
                || detail.contains("id_token") && version == OcppVersion::V1_6
            {
                continue;
            }
            assert!(
                report
                    .events
                    .iter()
                    .any(|event| event.detail.as_deref() == Some(detail)),
                "missing {detail} for {version:?}"
            );
        }
        assert_eq!(evidence.remote_pulls.load(Ordering::SeqCst), 1);
        let calls = evidence.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|action| **action == SimulatorAction::StartTransaction)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|action| **action == SimulatorAction::Authorize)
                .count(),
            3
        );
    }
}

#[test]
fn command_metadata_and_fault_combinations_fail_closed() {
    let missing_delivery = include_str!("../examples/resilience-1.6.toml")
        .replace("delivery_id = \"delivery-expired\"\n", "");
    assert_eq!(
        parse_scenario(&missing_delivery).unwrap_err().code,
        "invalid_command_fields"
    );

    let wrong_fault = include_str!("../examples/resilience-1.6.toml")
        .replace("kind = \"missing_response\"", "kind = \"response_delay\"");
    assert_eq!(
        parse_scenario(&wrong_fault).unwrap_err().code,
        "invalid_fault_delay"
    );
}
