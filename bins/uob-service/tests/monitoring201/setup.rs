use super::support::*;
use serde_json::{Value, json};

fn temperature() -> Value {
    json!({"component":{"name":"Connector","evse":{"id":1,"connectorId":1}},
        "variable":{"name":"Temperature"}})
}

fn monitor(value: &Value, kind: &str, severity: i32) -> Value {
    let mut entry = temperature();
    entry["value"] = value.clone();
    entry["type"] = json!(kind);
    entry["severity"] = json!(severity);
    entry
}

fn accepted(id: i32, kind: &str, severity: i32) -> Value {
    let mut entry = temperature();
    entry["id"] = json!(id);
    entry["status"] = json!("Accepted");
    entry["type"] = json!(kind);
    entry["severity"] = json!(severity);
    entry
}

fn ids(monitors: &Value) -> Vec<i64> {
    monitors
        .as_array()
        .unwrap()
        .iter()
        .map(|monitor| monitor["id"].as_i64().unwrap())
        .collect()
}

async fn report(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    id: &str,
    request_id: i32,
    fragments: &[&[i32]],
) -> Value {
    start_report(client, fixture, socket, id, request_id).await;
    for (index, monitors) in fragments.iter().enumerate() {
        let sequence = u32::try_from(index).unwrap();
        let last = index + 1 == fragments.len();
        let frame = fragment(
            &format!("{id}-{index}"),
            request_id,
            sequence,
            !last,
            monitors,
        );
        deliver(socket, frame).await;
    }
    settled(client, fixture, id).await
}

#[tokio::test]
async fn monitors_are_set_with_native_per_monitor_results() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let advertised =
        fixture.snapshot(&client, "station-a").await["capabilities"]["operations"].to_string();
    for action in [
        "SetVariableMonitoring",
        "ClearVariableMonitoring",
        "GetMonitoringReport",
        "SetMonitoringBase",
        "SetMonitoringLevel",
    ] {
        assert!(advertised.contains(&format!("\"{action}\"")), "{action}");
    }

    let payload = json!({"setMonitoringData":[monitor(&json!(80.0), "UpperThreshold", 3)]});
    let (call, set) = answered(
        &client,
        &fixture,
        &mut socket,
        command("set-1", "SetVariableMonitoring", payload.clone()),
        json!({"setMonitoringResult":[accepted(4010, "UpperThreshold", 3)]}),
    )
    .await;
    assert_eq!(call[3], payload);
    assert_eq!(set["lifecycle"]["accepted"], true);
    assert_eq!(set["monitoring_201"]["outcome"]["kind"], "set_monitors");
    assert_eq!(set["monitoring_201"]["outcome"]["monitors"][0]["id"], 4010);
    assert_eq!(
        set["monitoring_201"]["outcome"]["monitors"][0]["status"],
        "Accepted"
    );

    // The station reports its message limits before the bridge sends several monitors at once.
    learn_limits(&client, &fixture, &mut socket).await;
    let mut unknown = monitor(&json!(10), "LowerThreshold", 4);
    unknown["variable"] = json!({"name":"NoSuchVariable"});
    let mut unknown_result = unknown.clone();
    unknown_result["status"] = json!("UnknownVariable");
    unknown_result.as_object_mut().unwrap().remove("value");
    let (_, mixed) = answered(
        &client,
        &fixture,
        &mut socket,
        command(
            "set-2",
            "SetVariableMonitoring",
            json!({"setMonitoringData":[monitor(&json!(75.0), "UpperThreshold", 5), unknown]}),
        ),
        json!({"setMonitoringResult":[accepted(4011, "UpperThreshold", 5), unknown_result]}),
    )
    .await;
    // One refused monitor makes the command a protocol rejection; each result stays visible.
    assert_eq!(mixed["lifecycle"]["accepted"], false);
    let results = &mixed["monitoring_201"]["outcome"]["monitors"];
    assert_eq!(results[0]["id"], 4011);
    assert_eq!(results[1]["status"], "UnknownVariable");
    assert!(results[1]["id"].is_null());
}

#[tokio::test]
async fn a_report_arrives_in_fragments_and_clearing_removes_a_monitor_from_the_next_one() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    learn_limits(&client, &fixture, &mut socket).await;

    let full = report(
        &client,
        &fixture,
        &mut socket,
        "report-1",
        7001,
        &[&[4010], &[4011], &[4003]],
    )
    .await;
    let outcome = &full["monitoring_201"]["outcome"];
    assert_eq!(outcome["ack"], "Accepted");
    assert_eq!(outcome["report"]["state"], "complete");
    assert_eq!(ids(&outcome["report"]["monitors"]), [4010, 4011, 4003]);
    assert_eq!(outcome["report"]["fragments"].as_array().unwrap().len(), 3);
    assert_eq!(outcome["report"]["fragments"][2]["more"], false);
    assert_eq!(full["lifecycle"]["accepted"], true);

    // Each ID is judged on its own; a hard-wired monitor cannot be cleared.
    let (call, cleared) = answered(
        &client,
        &fixture,
        &mut socket,
        command(
            "clear-1",
            "ClearVariableMonitoring",
            json!({"id":[4010, 4012, 4003]}),
        ),
        json!({"clearMonitoringResult":[{"status":"Accepted","id":4010},
            {"status":"NotFound","id":4012},
            {"status":"Rejected","id":4003,"statusInfo":{"reasonCode":"HardWired"}}]}),
    )
    .await;
    assert_eq!(call[3], json!({"id":[4010, 4012, 4003]}));
    assert_eq!(cleared["lifecycle"]["accepted"], false);
    let items = cleared["monitoring_201"]["outcome"]["monitors"]
        .as_array()
        .unwrap();
    let statuses: Vec<_> = items
        .iter()
        .map(|item| {
            (
                item["id"].as_i64().unwrap(),
                item["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [(4010, "Accepted"), (4012, "NotFound"), (4003, "Rejected")]
    );
    assert_eq!(items[2]["reason_code"], "HardWired");

    let after = report(
        &client,
        &fixture,
        &mut socket,
        "report-2",
        7002,
        &[&[4011, 4003]],
    )
    .await;
    let monitors = &after["monitoring_201"]["outcome"]["report"]["monitors"];
    assert_eq!(ids(monitors), [4011, 4003]);
}

#[tokio::test]
async fn base_and_level_commands_carry_the_native_status() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    let (_, base) = answered(
        &client,
        &fixture,
        &mut socket,
        command(
            "base-1",
            "SetMonitoringBase",
            json!({"monitoringBase":"FactoryDefault"}),
        ),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(base["lifecycle"]["accepted"], true);
    assert_eq!(base["monitoring_201"]["outcome"]["kind"], "base");
    assert_eq!(base["monitoring_201"]["outcome"]["status"], "Accepted");

    let (_, unsupported) = answered(
        &client,
        &fixture,
        &mut socket,
        command(
            "base-2",
            "SetMonitoringBase",
            json!({"monitoringBase":"HardWiredOnly"}),
        ),
        json!({"status":"NotSupported"}),
    )
    .await;
    assert_eq!(unsupported["lifecycle"]["accepted"], false);
    assert_eq!(
        unsupported["monitoring_201"]["outcome"]["status"],
        "NotSupported"
    );

    let (call, level) = answered(
        &client,
        &fixture,
        &mut socket,
        command("level-1", "SetMonitoringLevel", json!({"severity":4})),
        json!({"status":"Rejected"}),
    )
    .await;
    assert_eq!(call[3], json!({"severity":4}));
    assert_eq!(level["lifecycle"]["accepted"], false);
    assert_eq!(level["monitoring_201"]["outcome"]["kind"], "level");
    assert_eq!(level["monitoring_201"]["outcome"]["status"], "Rejected");
}
