#[path = "issue88_host/mod.rs"]
mod host;
#[path = "../../../tests/ems-mqtt-contract-client/probe.rs"]
mod probe;

use std::{path::Path, time::Duration};
use uob_application::{
    OperationalStore, PageLimit, PendingDeliveryQuery, RetainedEventQuery, TargetDeliveryStore,
};
use uob_contracts::{TargetInstanceId, UtcTimestamp};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run scripts/test-ems-mqtt.sh to provision the isolated TLS/ACL broker"]
async fn issue88_integration() {
    let mut host = host::Host::start().await;
    let mut pump = host::Pump::start(&mut host);
    let demo: probe::Demo = toml::from_str(include_str!(
        "../../../tests/ems-mqtt-contract-client/demo.toml"
    ))
    .unwrap();
    let report = host::scenario::run(&host, Box::pin(run_consumer(&host, &mut pump, &demo))).await;
    // Both versions have one physical transaction start/stop despite duplicate,
    // expired and retained command submissions; the simulator report is independent of PUBACK.
    for station in ["station-a", "station-b"] {
        for action in ["start_transaction", "stop_transaction"] {
            assert_eq!(
                report
                    .events
                    .iter()
                    .filter(|event| event.station_id.as_deref() == Some(station)
                        && event.event == "step_passed"
                        && event.action == Some(action))
                    .count(),
                1,
                "{station}: physical effect repeated for {action}"
            );
        }
    }
    // Reports are only broker receipts, and the durable outbox must be finalized separately.
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let pending = host
                .store
                .read_pending_deliveries(PendingDeliveryQuery {
                    target_instance_id: TargetInstanceId::new("main").unwrap(),
                    target_configuration_revision: 1,
                    ready_at: serde_json::from_str::<UtcTimestamp>("\"2026-09-01T03:00:00Z\"")
                        .unwrap(),
                    limit: PageLimit::new(8).unwrap(),
                })
                .await
                .unwrap();
            if pending.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("all four journal events broker acknowledged and persisted");
    let delayed = host::freshness::commit_timed_observations(&host, &mut pump).await;
    host::security::reject_wrong_tls_and_credentials(&demo).await;
    let url = std::env::var("UOB_MQTT_BROKER_URL").unwrap();
    let ca = std::env::var("UOB_MQTT_CA_FILE").unwrap();
    let reader = std::env::var("UOB_MQTT_READER_USER").unwrap();
    let password = std::env::var("UOB_MQTT_READER_PASSWORD_FILE").unwrap();
    let resumed = probe::run_with_outage(
        &url,
        Path::new(&ca),
        &reader,
        Path::new(&password),
        &delayed,
        host::outage::BrokerPause::from_runner().interrupt(),
    )
    .await
    .unwrap();
    assert_eq!(resumed.status, "passed");
    assert!(
        resumed.connected
            && resumed.target_online
            && resumed.consumer_reconnects >= 1
            && resumed.retained_after_reconnect
            && resumed.subscriptions_acknowledged >= 6
    );
    assert!(!resumed.scenarios[0].current_at_receive);
    assert_eq!(resumed.scenarios[0].current_after_reconnect, Some(false));
    assert!(resumed.scenarios[1].current_at_receive);
    assert_eq!(resumed.scenarios[1].current_after_reconnect, Some(true));
    println!(
        "independent MQTT consumer after broker interruption: {}",
        serde_json::to_string(&resumed).unwrap()
    );
}

async fn run_consumer(host: &host::Host, pump: &mut host::Pump, demo: &probe::Demo) {
    // Wait for retained boot snapshot PUBACKs before an independent reader subscribes;
    // a live publication is not a retained read.
    pump.expect_initial_snapshots().await;
    let initial = probe::run(
        &std::env::var("UOB_MQTT_BROKER_URL").unwrap(),
        Path::new(&std::env::var("UOB_MQTT_CA_FILE").unwrap()),
        &std::env::var("UOB_MQTT_READER_USER").unwrap(),
        Path::new(&std::env::var("UOB_MQTT_READER_PASSWORD_FILE").unwrap()),
        demo,
        false,
        false,
    )
    .await
    .unwrap();
    assert!(initial.target_online && initial.connected && initial.subscriptions_acknowledged >= 6);
    host::security::reject_reader_command(host, demo).await;
    let executable = std::env::var("UOB_MQTT_EXAMPLE_PATH").expect("runner-built CLI executable");
    let output = tokio::time::timeout(
        Duration::from_secs(110),
        tokio::process::Command::new(executable)
            .arg(std::env::var("UOB_MQTT_BROKER_URL").unwrap())
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/ems-mqtt-contract-client/demo.toml"
            ))
            .arg("--exercise")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("CLI deadline")
    .expect("CLI process");
    assert!(
        output.status.success(),
        "CLI failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["connected"], true);
    assert_eq!(result["target_online"], true);
    assert_eq!(result["scenarios"].as_array().unwrap().len(), 2);
    for (index, protocol) in [(0, "ocpp16"), (1, "ocpp201")] {
        let evidence = &result["scenarios"][index];
        assert_eq!(evidence["protocol"], protocol);
        assert_eq!(evidence["broker_acknowledgements"], 6);
        assert_eq!(evidence["correlated_results"], 7);
        assert_eq!(evidence["observed_effects"], 2);
        for field in [
            "descriptor",
            "exact_value",
            "retained_state",
            "expired_rejected",
            "retained_replay_rejected",
            "deduplicated",
        ] {
            assert_eq!(evidence[field], true, "{protocol} missing {field}");
        }
        assert_eq!(evidence["event_ids"].as_array().unwrap().len(), 2);
    }
    assert_journal_matches_cli(host, &result).await;
}

async fn assert_journal_matches_cli(host: &host::Host, cli: &serde_json::Value) {
    let page = host
        .store
        .read_snapshots(uob_application::SnapshotQuery {
            after: None,
            limit: PageLimit::new(8).unwrap(),
        })
        .await
        .unwrap();
    for snapshot in page.items {
        let station = snapshot.station.station_id.as_str();
        assert_eq!(
            snapshot.transactions.len(),
            1,
            "duplicate caused a second transaction"
        );
        let events = host
            .store
            .read_retained_events(RetainedEventQuery {
                resource: snapshot.transactions[0].resource.clone(),
                after: None,
                limit: PageLimit::new(8).unwrap(),
            })
            .await
            .unwrap()
            .events;
        assert_eq!(
            events.len(),
            2,
            "exactly two committed journal effects for {station}"
        );
        let received = cli["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["station"] == station)
            .unwrap()["event_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect::<Vec<_>>();
        let expected = events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            received, expected,
            "consumer event identities differ from durable journal"
        );
        assert_eq!(
            serde_json::to_value(&snapshot.transactions[0]).unwrap()["state"],
            "ended"
        );
    }
}
