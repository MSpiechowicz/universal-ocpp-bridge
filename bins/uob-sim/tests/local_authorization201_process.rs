use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
#[path = "local_authorization201/process.rs"]
mod common;
use common::*;

#[tokio::test]
async fn binary_kill_restart_denied_boot_recovery_delivers_exact_original_native_facts() {
    let directory = PrivateDirectory::new();
    let state = directory.0.join("state.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.write("config.toml", &format!("schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='2.0.1'\nrequest_timeout_ms=1000\nreconnect=false\n[stations.local_authorization]\nprivate_state_file='{}'\n",state.display()));
    let first = prefix()
        + &step("connect", "connect", "")
        + &native_boot("boot", "Accepted")
        + &step(
            "await-full",
            "await_local_authorization",
            "expect_response={listVersion=7,listEntries=1}",
        )
        + &step("offline", "csms_offline", "")
        + &facts()
        + "\n[[steps]]\nid='park'\nstation='alpha'\naction='wait'\ntimeout_ms=61000\nduration_ms=60000\n";
    let first = directory.write("first.toml", &first);
    let second = prefix()
        + &step("connect", "connect", "")
        + &native_boot("denied", "Rejected")
        + &step(
            "retained",
            "assert_local_authorization",
            "expect_response={listVersion=7,listEntries=1,offlineRecords=2,uncertainRecords=0}",
        )
        + &step("no-replay-window", "wait", "duration_ms=150")
        + &native_boot("recovery", "Accepted")
        + &step(
            "delivered",
            "assert_local_authorization",
            "expect_response={offlineRecords=0,uncertainRecords=0}",
        )
        + &step("disconnect", "disconnect", "");
    let second = directory.write("second.toml", &second);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let full: Value = serde_json::from_str(include_str!(
            "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/local-list-full.json"
        ))
        .unwrap();
        send(&mut socket, full).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        closed(&mut socket).await;
        let mut socket = accept(&listener).await;
        let boot = receive(&mut socket).await;
        send(&mut socket,json!([3,boot[1],{"status":"Rejected","currentTime":"2026-10-04T00:00:00Z","interval":1}])).await;
        assert!(
            timeout(Duration::from_millis(75), receive(&mut socket))
                .await
                .is_err()
        );
        let boot = receive(&mut socket).await;
        assert_eq!(boot[2], "BootNotification");
        send(&mut socket,json!([3,boot[1],{"status":"Accepted","currentTime":"2026-10-04T00:00:00Z","interval":60}])).await;
        for fact in [start_fact(), end_fact()] {
            let request = receive(&mut socket).await;
            assert_eq!(request[2], "TransactionEvent");
            assert_eq!(request[3], fact);
            send(&mut socket, json!([3, request[1], {}])).await;
        }
        closed(&mut socket).await;
    });
    let mut process = Process::start(&config, &first);
    await_queue(&state, 2).await;
    assert_private_output(&process.kill());
    let persisted: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(persisted["offline"][0]["payload"], start_fact());
    assert_eq!(persisted["offline"][1]["payload"], end_fact());
    let mut recovered = Process::start(&config, &second);
    let output = recovered.finish().await;
    assert_private_output(&output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    timeout(BOUND, peer).await.unwrap().unwrap();
    assert_eq!(
        fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn binary_dropped_native_ack_commits_once_without_replaying_csms_update() {
    let directory = PrivateDirectory::new();
    let state = directory.0.join("state.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.write("config.toml", &format!("schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='2.0.1'\nrequest_timeout_ms=1000\nreconnect=false\n[stations.local_authorization]\nprivate_state_file='{}'\n",state.display()));
    let scenario = prefix()
        + &step("connect", "connect", "")
        + &native_boot("boot", "Accepted")
        + &step("arm-drop", "drop_local_reply", "")
        + &step(
            "full-committed",
            "await_local_authorization",
            "expect_response={listVersion=7,listEntries=1}",
        )
        + &step("reconnect", "csms_reconnect", "")
        + &step(
            "persisted",
            "assert_local_authorization",
            "expect_response={listVersion=7,listEntries=1,offlineRecords=0}",
        )
        + &step("disconnect", "disconnect", "");
    let scenario = directory.write("scenario.toml", &scenario);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let full: Value = serde_json::from_str(include_str!(
            "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/local-list-full.json"
        ))
        .unwrap();
        send(&mut socket, full).await;
        closed(&mut socket).await;
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        // The new socket emits only Boot, never replays SendLocalList or ClearCache.
        closed(&mut socket).await;
    });
    let mut process = Process::start(&config, &scenario);
    let output = process.finish().await;
    assert_private_output(&output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    timeout(BOUND, peer).await.unwrap().unwrap();
}

#[tokio::test]
async fn binary_native_reset_ack_closes_recovers_and_performs_fresh_boot() {
    let directory = PrivateDirectory::new();
    let state = directory.0.join("state.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.write("config.toml",&format!("schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='2.0.1'\nreconnect=true\n[stations.local_authorization]\nprivate_state_file='{}'\n",state.display()));
    let scenario = prefix()
        + &step("connect", "connect", "")
        + &native_boot("boot", "Accepted")
        + &step(
            "installed",
            "await_local_authorization",
            "expect_response={listVersion=7,listEntries=1}",
        )
        + &step("actual-reset", "await_reboot", "")
        + &step("fresh-boot-window", "wait", "duration_ms=2000")
        + &step(
            "retained",
            "assert_local_authorization",
            "expect_response={listVersion=7,listEntries=1}",
        )
        + &step("disconnect", "disconnect", "");
    let scenario = directory.write("scenario.toml", &scenario);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let full: Value = serde_json::from_str(include_str!(
            "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/local-list-full.json"
        ))
        .unwrap();
        send(&mut socket, full).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        send(
            &mut socket,
            json!([2,"native-reset","Reset",{"type":"Immediate"}]),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"native-reset",{"status":"Accepted"}])
        );
        closed(&mut socket).await;
        let mut recovered = accept(&listener).await;
        boot(&mut recovered).await;
        send(
            &mut recovered,
            json!([2, "fresh-query", "GetLocalListVersion", {}]),
        )
        .await;
        assert_eq!(receive(&mut recovered).await[2]["versionNumber"], 7);
        closed(&mut recovered).await;
    });
    let mut process = Process::start(&config, &scenario);
    let output = process.finish().await;
    assert_private_output(&output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    timeout(BOUND, peer).await.unwrap().unwrap();
}

#[tokio::test]
async fn binary_reports_actual_entry_count_separately_from_capacity_and_caseless_identity() {
    let directory = PrivateDirectory::new();
    let state = directory.0.join("state.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.write("report-config.toml", &format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='2.0.1'\nrequest_timeout_ms=1000\nreconnect=false\n[stations.local_authorization]\nprivate_state_file='{}'\n", state.display()));
    let scenario = prefix()
        + &step("connect", "connect", "")
        + &native_boot("boot", "Accepted")
        + &step(
            "reports-finished",
            "await_local_authorization",
            "expect_response={listVersion=8,listEntries=1}",
        )
        + &step("disconnect", "disconnect", "");
    let scenario = directory.write("reports.toml", &scenario);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        send(&mut socket, json!([2,"inventory-before","GetBaseReport",{"requestId":-17,"reportBase":"FullInventory"}])).await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"inventory-before",{"status":"Accepted"}])
        );
        let report = receive(&mut socket).await;
        assert_controller_report(&report, -17, "0", 7);
        send(&mut socket, json!([3, report[1], {}])).await;
        let full: Value = serde_json::from_str(include_str!(
            "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/local-list-full.json"
        ))
        .unwrap();
        send(&mut socket, full).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        send(&mut socket, json!([2,"inventory-after","GetBaseReport",{"requestId":18,"reportBase":"FullInventory"}])).await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"inventory-after",{"status":"Accepted"}])
        );
        let report = receive(&mut socket).await;
        assert_controller_report(&report, 18, "1", 7);
        send(&mut socket, json!([3, report[1], {}])).await;
        send(&mut socket, json!([2,"caseless-query","GetVariables",{"getVariableData":[
            {"component":{"name":"localauthlistCTRLR"},"variable":{"name":"eNTRIES"}},
            {"component":{"name":"AUTHCACHEctrlr"},"variable":{"name":"enabled"}},
            {"component":{"name":"LocalAuthListCtrlr","instance":"other"},"variable":{"name":"Entries"}},
            {"component":{"name":"LocalAuthLiſtCtrlr"},"variable":{"name":"Entrieſ"}},
            {"component":{"name":"LocalAuthLiſtCtrlr","instance":"other"},"variable":{"name":"Entrieſ"}}]}])).await;
        let result = receive(&mut socket).await;
        assert_eq!(
            result[2]["getVariableResult"][0]["attributeStatus"],
            "Accepted"
        );
        assert_eq!(result[2]["getVariableResult"][0]["attributeValue"], "1");
        assert_eq!(result[2]["getVariableResult"][1]["attributeValue"], "true");
        assert_eq!(
            result[2]["getVariableResult"][2]["attributeStatus"],
            "UnknownVariable"
        );
        assert_eq!(
            result[2]["getVariableResult"][3]["attributeStatus"],
            "Accepted"
        );
        assert_eq!(result[2]["getVariableResult"][3]["attributeValue"], "1");
        assert_eq!(
            result[2]["getVariableResult"][4]["attributeStatus"],
            "UnknownVariable"
        );
        send(
            &mut socket,
            json!([2,"selected-report","GetReport",{"requestId":19,"componentVariable":[
            {"component":{"name":"LocalAuthLiſtCTRLR"},"variable":{"name":"Entrieſ"}}]}]),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"selected-report",{"status":"Accepted"}])
        );
        let report = receive(&mut socket).await;
        assert_controller_report(&report, 19, "1", 1);
        send(&mut socket, json!([3, report[1], {}])).await;
        send(&mut socket, json!([2,"scoped-report","GetReport",{"requestId":20,"componentVariable":[
            {"component":{"name":"LocalAuthLiſtCtrlr","evse":{"id":1}},"variable":{"name":"Entrieſ"}}]}])).await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"scoped-report",{"status":"EmptyResultSet"}])
        );
        send(&mut socket, json!([2,"report-completion-barrier","SendLocalList",{"versionNumber":8,"updateType":"Differential"}])).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        closed(&mut socket).await;
    });
    let mut process = Process::start(&config, &scenario);
    let output = process.finish().await;
    assert_private_output(&output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    timeout(BOUND, peer).await.unwrap().unwrap();
}

fn assert_controller_report(report: &Value, request_id: i64, actual: &str, count: usize) {
    assert_eq!(report[0], 2);
    assert_eq!(report[2], "NotifyReport");
    assert_eq!(report[3]["requestId"], request_id);
    assert_eq!(report[3]["seqNo"], 0);
    assert_eq!(report[3]["tbc"], false);
    let generated = report[3]["generatedAt"].as_str().unwrap();
    time::OffsetDateTime::parse(generated, &time::format_description::well_known::Rfc3339).unwrap();
    let data = report[3]["reportData"].as_array().unwrap();
    assert_eq!(data.len(), count);
    let entries = data
        .iter()
        .find(|row| {
            row["component"]["name"] == "LocalAuthListCtrlr" && row["variable"]["name"] == "Entries"
        })
        .unwrap();
    assert_eq!(entries["variableAttribute"][0]["type"], "Actual");
    assert_eq!(entries["variableAttribute"][0]["value"], actual);
    assert_eq!(entries["variableCharacteristics"]["maxLimit"], 256);
    assert_eq!(entries["variableCharacteristics"]["dataType"], "integer");
    assert_eq!(
        entries["variableCharacteristics"]["supportsMonitoring"],
        false
    );
    let wire = report.to_string().to_ascii_lowercase();
    assert!(!wire.contains("marker-112"));
}
