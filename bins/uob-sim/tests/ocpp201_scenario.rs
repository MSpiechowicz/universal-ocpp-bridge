#![allow(clippy::result_large_err)]

use std::sync::Arc;

use uob_sim::scenario::{
    ConnectFuture, ScenarioConnector, ScenarioRunner, cancellation_pair, parse_configuration,
    parse_scenario,
};
use uob_sim::{
    ClientFuture, OcppVersion, ProtocolClient, SimulatorAction, SimulatorCall,
    SimulatorClientConfig,
};

const CONFIGURATION: &str = r#"
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
connectors = [1, 2]
"#;

struct FakeConnector;

impl ScenarioConnector for FakeConnector {
    fn connect(&self, configuration: SimulatorClientConfig) -> ConnectFuture<'_> {
        Box::pin(async move {
            Ok(Box::new(FakeClient {
                version: configuration.version,
            }) as Box<dyn ProtocolClient>)
        })
    }
}

struct FakeClient {
    version: OcppVersion,
}

impl ProtocolClient for FakeClient {
    fn version(&self) -> OcppVersion {
        self.version
    }

    fn heartbeat(&self) -> ClientFuture<'_, String> {
        Box::pin(async { Ok("2026-09-01T00:00:00Z".to_owned()) })
    }

    fn call(&self, call: SimulatorCall) -> ClientFuture<'_, serde_json::Value> {
        Box::pin(async move {
            Ok(match call.action {
                SimulatorAction::BootNotification => serde_json::json!({
                    "currentTime": "2026-09-01T00:00:00Z",
                    "interval": 60,
                    "status": "Accepted"
                }),
                SimulatorAction::Authorize => {
                    let status = if call.payload.pointer("/idToken/idToken")
                        == Some(&serde_json::Value::String("DENIED".to_owned()))
                    {
                        "Invalid"
                    } else {
                        "Accepted"
                    };
                    serde_json::json!({"idTokenInfo": {"status": status}})
                }
                SimulatorAction::StartTransaction => {
                    serde_json::json!({"idTokenInfo": {"status": "Accepted"}})
                }
                SimulatorAction::StatusNotification
                | SimulatorAction::MeterValues
                | SimulatorAction::StopTransaction => serde_json::json!({}),
            })
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

async fn run(source: &str) -> uob_sim::scenario::RunReport {
    let configuration = parse_configuration(CONFIGURATION).unwrap();
    let scenario = parse_scenario(source).unwrap();
    ScenarioRunner::new(
        Arc::new(FakeConnector),
        Arc::new(uob_sim::scenario::RealClock),
    )
    .run(
        &configuration,
        &scenario,
        scenario.seed,
        cancellation_pair().1,
    )
    .await
}

#[tokio::test]
async fn multi_evse_charging_scenario_preserves_transaction_resource_and_quality() {
    let report = run(include_str!("../examples/charging-2.0.1.toml")).await;

    assert!(report.failure.is_none(), "{:?}", report.failure);
    for action in [
        "boot",
        "authorize",
        "status",
        "start_transaction",
        "meter_values",
        "stop_transaction",
    ] {
        assert!(
            report
                .events
                .iter()
                .any(|event| event.event == "step_passed" && event.action == Some(action))
        );
    }
}

#[tokio::test]
async fn duplicate_and_out_of_order_sequences_fail_independently() {
    let source = include_str!("../examples/charging-2.0.1.toml");
    let duplicate = source.replacen("seqNo = 1", "seqNo = 0", 1);
    assert_eq!(
        run(&duplicate).await.failure.unwrap().code,
        "duplicate_transaction_sequence"
    );

    let out_of_order = source.replacen("seqNo = 1", "seqNo = 2", 1);
    assert_eq!(
        run(&out_of_order).await.failure.unwrap().code,
        "out_of_order_transaction_sequence"
    );
}

#[tokio::test]
async fn rejected_authorization_cannot_start_a_201_transaction() {
    let source = include_str!("../examples/charging-2.0.1.toml")
        .replace("LOCAL-USER-201", "DENIED")
        .replace(
            "expect_response = { idTokenInfo = { status = \"Accepted\" } }",
            "expect_response = { idTokenInfo = { status = \"Invalid\" } }",
        );

    assert_eq!(run(&source).await.failure.unwrap().code, "not_authorized");
}

#[tokio::test]
async fn incomplete_meter_quality_and_flattened_evse_identity_fail_closed() {
    let source = include_str!("../examples/charging-2.0.1.toml");
    let missing_quality = source.replacen("phase = \"L1-N\", ", "", 1);
    assert_eq!(
        run(&missing_quality).await.failure.unwrap().code,
        "missing_meter_data_quality"
    );

    let wrong_resource = source.replacen(
        "evse = { id = 2, connectorId = 1 }, meterValue",
        "evse = { id = 1, connectorId = 1 }, meterValue",
        1,
    );
    assert_eq!(
        run(&wrong_resource).await.failure.unwrap().code,
        "transaction_not_active"
    );
}
