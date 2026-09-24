#[path = "issue87_host/mod.rs"]
mod host;
use std::sync::Arc;
use std::time::Duration;
use uob_application::{
    DeliveryAttempt, DeliveryAttemptResolution, DeliveryOutcome, OperationalStore, PageLimit,
    PendingDeliveryQuery, RetainedEventQuery, TargetDelivery, TargetDeliveryClass,
    TargetDeliveryStore, TargetMessage,
};
use uob_contracts::{TargetInstanceId, UtcTimestamp};
use uob_sim::scenario::{
    ActionKind, LiveRun, RunReport, ScenarioDefinition, ScenarioRunner, SimulatorConfiguration,
    cancellation_pair, parse_configuration, parse_scenario,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_example_and_simulator_sessions_dispatch_fresh_remote_charging_flows() {
    let mut host = host::Host::start().await;
    reject_unauthorized_proxy_peers(&host.protocol.proxy_201_address).await;
    let configuration = parse_configuration(&format!(
        r#"
schema_version = 1
station_capacity = 2
[[stations]]
id = "station-a"
endpoint = "{}"
ocpp_version = "1.6"
connectors = [1]
command_capacity = 8
step_capacity = 16
request_timeout_ms = 5000
[[stations]]
id = "station-b"
endpoint = "{}"
ocpp_version = "2.0.1"
command_capacity = 8
step_capacity = 16
request_timeout_ms = 5000
[[stations.evses]]
id = 1
connectors = [1]
"#,
        host.socket, host.protocol.proxy_201_address
    ))
    .unwrap();
    let scenario = parse_scenario(include_str!(
        "../../../tests/ems-http-contract-client/remote.toml"
    ))
    .unwrap();
    let repeat = scenario.clone();
    let first = run_exercise_session(&host.base, &configuration, scenario).await;
    assert_locally_exposed_outbox(&mut host).await;
    let second = repeat_scenario(repeat, &first);
    run_exercise_session(&host.base, &configuration, second).await;
    assert_locally_exposed_outbox(&mut host).await;
}

fn repeat_scenario(mut scenario: ScenarioDefinition, first: &RunReport) -> ScenarioDefinition {
    // Station event IDs use call sequence numbers. Advance the next start past
    // every call from the first start through the first stop, including heartbeats.
    for station in ["16", "201"] {
        let boot = scenario
            .steps
            .iter()
            .position(|step| step.id == format!("boot-{station}"))
            .unwrap();
        let start = scenario
            .steps
            .iter()
            .position(|step| step.id == format!("transaction-start-{station}"))
            .unwrap();
        let stop_index = scenario
            .steps
            .iter()
            .position(|step| step.id == format!("transaction-stop-{station}"))
            .unwrap();
        let calls_after_start = scenario.steps[start + 1..=stop_index]
            .iter()
            .filter(|step| {
                step.station == scenario.steps[boot].station
                    && matches!(
                        step.action,
                        ActionKind::Boot
                            | ActionKind::Authorize
                            | ActionKind::Status
                            | ActionKind::StartTransaction
                            | ActionKind::MeterValues
                            | ActionKind::StopTransaction
                            | ActionKind::Heartbeat
                    )
            })
            .count();
        let mut heartbeat = scenario.steps[boot].clone();
        heartbeat.action = ActionKind::Heartbeat;
        heartbeat.payload = None;
        heartbeat.fixture_id = None;
        heartbeat.expect_message = None;
        heartbeat.expect_response = None;
        heartbeat.expect_detail = None;
        heartbeat.expect_event = Some("heartbeat_result".to_owned());
        for index in 0..=calls_after_start {
            let mut repeated_heartbeat = heartbeat.clone();
            repeated_heartbeat.id = format!("repeat-heartbeat-{station}-{index}");
            scenario.steps.insert(boot + 1 + index, repeated_heartbeat);
        }
    }

    let first_16_id = first
        .events
        .iter()
        .find(|event| {
            event.event == "step_passed" && event.step_id.as_deref() == Some("transaction-start-16")
        })
        .and_then(|event| event.detail.as_deref())
        .and_then(|detail| serde_json::from_str::<serde_json::Value>(detail).ok())
        .and_then(|response| response["transactionId"].as_i64())
        .expect("first OCPP 1.6 StartTransaction response");
    scenario
        .steps
        .iter_mut()
        .find(|step| step.id == "transaction-stop-16")
        .unwrap()
        .payload
        .as_mut()
        .unwrap()["transactionId"] = serde_json::json!(first_16_id + 1);

    let next_201_id = {
        let started = scenario
            .steps
            .iter()
            .find(|step| step.id == "transaction-start-201")
            .unwrap();
        format!(
            "{}-repeat",
            started.payload.as_ref().unwrap()["transactionInfo"]["transactionId"]
                .as_str()
                .unwrap()
        )
    };
    for id in ["transaction-start-201", "transaction-stop-201"] {
        scenario
            .steps
            .iter_mut()
            .find(|step| step.id == id)
            .unwrap()
            .payload
            .as_mut()
            .unwrap()["transactionInfo"]["transactionId"] = serde_json::json!(next_201_id);
    }
    scenario
}

async fn run_exercise_session(
    base: &str,
    configuration: &SimulatorConfiguration,
    scenario: ScenarioDefinition,
) -> RunReport {
    let live = LiveRun::new(&scenario);
    let progress = live.clone();
    let configuration = configuration.clone();
    let (_, cancellation) = cancellation_pair();
    let simulator = tokio::spawn(async move {
        ScenarioRunner::default()
            .run_controlled(&configuration, &scenario, 87, cancellation, live)
            .await
    });
    // The previous session's committed snapshot can still look connected.
    // Wait for both fresh boots before the external client begins inventory.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let steps = progress.snapshot();
            if ["boot-16", "boot-201"].iter().all(|id| {
                steps
                    .iter()
                    .any(|step| step.step_id == *id && step.status == "passed")
            }) {
                break;
            }
            assert!(
                !simulator.is_finished(),
                "simulator ended before both boots"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("simulator completed both boots");
    assert_example(base).await;
    let report = tokio::time::timeout(Duration::from_secs(5), simulator)
        .await
        .unwrap()
        .unwrap();
    assert!(
        report.failure.is_none(),
        "simulator failed: {:?}",
        report.failure
    );
    for (station, readiness_step) in [
        ("station-a", "ready-for-stop-16"),
        ("station-b", "ready-for-stop-201"),
    ] {
        for action in [
            "await_remote_start",
            "start_transaction",
            "await_remote_stop",
            "stop_transaction",
        ] {
            assert!(
                report
                    .events
                    .iter()
                    .any(|e| e.station_id.as_deref() == Some(station)
                        && e.event == "step_passed"
                        && e.action == Some(action)),
                "{station} missing {action}: {:?}",
                report.events
            );
        }
        assert!(
            report
                .events
                .iter()
                .any(|e| e.station_id.as_deref() == Some(station)
                    && e.event == "step_passed"
                    && e.step_id.as_deref() == Some(readiness_step)),
            "{station} did not acknowledge readiness after transaction start"
        );
        for action in ["await_remote_start", "await_remote_stop"] {
            assert!(
                report
                    .events
                    .iter()
                    .any(|e| e.station_id.as_deref() == Some(station)
                        && e.action == Some(action)
                        && e.detail
                            .as_deref()
                            .is_some_and(|detail| detail.contains("\"accepted\":true"))),
                "{station} did not accept {action}"
            );
        }
    }
    report
}

async fn assert_example(base: &str) {
    let executable =
        std::env::var("UOB_EXAMPLE_PATH").expect("build the example and set UOB_EXAMPLE_PATH");
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let (seen, mut received) = tokio::sync::mpsc::unbounded_channel();
    let recorder = tokio::spawn(async move {
        while let Ok((stream, _)) = proxy.accept().await {
            let _ = seen.send(());
            drop(stream); // Fail an inherited-proxy connection immediately.
        }
    });
    let output = tokio::time::timeout(
        Duration::from_secs(65),
        tokio::process::Command::new(executable)
            .arg(base)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/ems-http-contract-client/demo.toml"
            ))
            .arg("--exercise")
            .env("UOB_EMS_TOKEN", host::READER)
            .env("UOB_EMS_OPERATOR_TOKEN", host::OPERATOR)
            .env("HTTP_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env("all_proxy", &proxy_url)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env_remove("REQUEST_METHOD")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("bounded example process")
    .expect("example output");
    let intercepted = tokio::time::timeout(Duration::from_millis(100), received.recv()).await;
    recorder.abort();
    assert!(
        !matches!(intercepted, Ok(Some(()))),
        "loopback example sent an API request to the inherited proxy"
    );
    assert!(
        output.status.success(),
        "example stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    println!("example process: {result}");
    let scenarios = result["scenarios"]
        .as_array()
        .expect("machine-readable scenario array");
    assert_eq!(scenarios.len(), 2);
    for (index, protocol, mode) in [(0, "ocpp16", "absent"), (1, "ocpp201", "stalled")] {
        let scenario = &scenarios[index];
        assert_eq!(scenario["protocol"], protocol);
        let points = scenario["points"].as_u64().expect("point inventory count");
        assert!(
            points > 2,
            "{protocol} must traverse multiple limit=2 pages"
        );
        assert_eq!(scenario["subscription_mode"], mode);
        assert_eq!(scenario["admission_http_status"], 202);
        assert_eq!(scenario["observed_effects"], 2);
        assert_eq!(scenario["sse"]["durable"], 2);
        assert_eq!(scenario["sse"]["resumed"], true);
        for key in [
            "protocol_accepted",
            "point_value",
            "deduplicated",
            "reader_denied",
            "unauthorized_data_denied",
            "expired_rejected",
            "expired_cursor_recovered",
        ] {
            assert_eq!(scenario[key], true, "{protocol} missing {key}");
        }
    }
}

async fn assert_locally_exposed_outbox(host: &mut host::Host) {
    let ready_at: UtcTimestamp = serde_json::from_str("\"2026-09-01T03:00:00Z\"").unwrap();
    for _batch in 0..2 {
        let pending = host
            .store
            .read_pending_deliveries(PendingDeliveryQuery {
                target_instance_id: TargetInstanceId::new("main").unwrap(),
                target_configuration_revision: 1,
                ready_at,
                limit: PageLimit::new(8).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(pending.len(), 2, "one ready committed event per station");
        for scheduled in pending {
            let delivery = scheduled.delivery;
            let page = host
                .store
                .read_retained_events(RetainedEventQuery {
                    resource: delivery.ordering_key.clone(),
                    after: None,
                    limit: PageLimit::new(8).unwrap(),
                })
                .await
                .unwrap();
            let event = page
                .events
                .into_iter()
                .find(|event| event.event_id == delivery.event_id)
                .expect("outbox event exists in committed journal");
            let expected = delivery.delivery_id.clone();
            host.deliveries
                .send(TargetDelivery {
                    delivery_id: delivery.delivery_id,
                    target_instance_id: delivery.target_instance_id,
                    target_configuration_revision: delivery.target_configuration_revision,
                    station_ordering_key: delivery.ordering_key,
                    deadline: delivery.deadline,
                    class: TargetDeliveryClass::Durable,
                    message: Arc::new(TargetMessage::DomainEvent(event)),
                })
                .await
                .unwrap();
            let report = tokio::time::timeout(Duration::from_secs(5), host.reports.recv())
                .await
                .unwrap()
                .expect("local target delivery report");
            assert_eq!(report.delivery_id, expected);
            assert!(
                matches!(report.outcome, DeliveryOutcome::LocallyExposed { ref surface }
                if surface == "/bridge/v1"),
                "never assert remote EMS consumption"
            );
            host.store
                .record_delivery_attempt(DeliveryAttempt {
                    report,
                    resolution: DeliveryAttemptResolution::Final,
                })
                .await
                .unwrap();
        }
    }
}

async fn reject_unauthorized_proxy_peers(endpoint: &str) {
    use tokio_tungstenite::tungstenite::{
        client::IntoClientRequest, http::header::SEC_WEBSOCKET_PROTOCOL,
    };

    let mut wrong_path = reqwest::Url::parse(endpoint).unwrap();
    wrong_path.set_path("/ocpp/station-b");
    for (url, protocol) in [
        (wrong_path.as_str(), "ocpp2.0.1"),
        (endpoint, "ocpp2.0.1,ocpp1.6"),
    ] {
        let mut request = url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert(SEC_WEBSOCKET_PROTOCOL, protocol.parse().unwrap());
        let Err(rejected) = tokio_tungstenite::connect_async(request).await else {
            panic!("unauthorized proxy handshake accepted")
        };
        assert!(
            matches!(rejected, tokio_tungstenite::tungstenite::Error::Http(response)
            if response.status() == tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN),
            "unauthorized proxy handshake must be rejected"
        );
    }
}
