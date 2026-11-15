//! The monitoring limits and monitorable variables through `GetVariables` and the reports.
use crate::monitoring201_support::{assert_valid, connector};
use crate::socket::{accept, boot, send};
use crate::{call, expect_call, run, station};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use uob_sim::ProtocolClient;

fn variable(component: &Value, name: &str, instance: Option<&str>) -> Value {
    let mut variable = json!({"name": name});
    if let Some(instance) = instance {
        variable["instance"] = json!(instance);
    }
    json!({"component": component, "variable": variable})
}

#[tokio::test]
async fn the_monitoring_limits_and_values_are_readable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let controller = json!({"name": "MonitoringCtrlr"});
        let query = json!({"getVariableData": [
            variable(&controller, "ItemsPerMessage", Some("SetVariableMonitoring")),
            variable(&controller, "ItemsPerMessage", Some("ClearVariableMonitoring")),
            variable(&controller, "BytesPerMessage", Some("SetVariableMonitoring")),
            variable(&controller, "BytesPerMessage", Some("ClearVariableMonitoring")),
            variable(&controller, "ActiveMonitoringLevel", None),
            variable(&controller, "ActiveMonitors", None),
            variable(&controller, "ItemsPerMessage", None),
            variable(&connector(), "Temperature", None),
            variable(&json!({"name": "Heater"}), "Temperature", None)]});
        let reply = call(&mut socket, "get", "GetVariables", query).await;
        assert_valid("GetVariablesResponse", &reply[2]);
        let results = reply[2]["getVariableResult"].as_array().unwrap();
        let facts: Vec<_> = results
            .iter()
            .map(|result| {
                (
                    result["attributeStatus"].as_str().unwrap(),
                    result.get("attributeValue").and_then(Value::as_str),
                )
            })
            .collect();
        assert_eq!(
            facts,
            [
                ("Accepted", Some("3")),
                ("Accepted", Some("2")),
                ("Accepted", Some("1000")),
                ("Accepted", Some("100")),
                ("Accepted", Some("9")),
                ("Accepted", Some("2")),
                ("UnknownVariable", None),
                ("Accepted", Some("40")),
                ("UnknownComponent", None),
            ]
        );
    };
    let fields = json!({"items_per_message_set": 3, "items_per_message_clear": 2,
        "bytes_per_message_set": 1000, "bytes_per_message_clear": 100});
    let client = run(peer, station(&listener, fields)).await;
    client.abort();
}

#[tokio::test]
async fn reports_say_which_variables_support_monitoring() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let reply = call(
            &mut socket,
            "base",
            "GetBaseReport",
            json!({"requestId": 8, "reportBase": "FullInventory"}),
        )
        .await;
        assert_eq!(reply[2]["status"], "Accepted");
        let report = expect_call(&mut socket, "NotifyReport").await;
        let data = report["reportData"].as_array().unwrap();
        let supports = |component: &str, variable: &str| {
            data.iter()
                .find(|entry| {
                    entry["component"]["name"] == component && entry["variable"]["name"] == variable
                })
                .map(|entry| entry["variableCharacteristics"]["supportsMonitoring"].clone())
        };
        // The model alone decides: monitorable variables say true, everything else false.
        assert_eq!(supports("Connector", "Temperature"), Some(json!(true)));
        assert_eq!(supports("ChargingStation", "Tamper"), Some(json!(false)));
        assert_eq!(
            supports("MonitoringCtrlr", "ActiveMonitors"),
            Some(json!(false))
        );
        assert_eq!(
            supports("LocalAuthListCtrlr", "Entries"),
            Some(json!(false))
        );
        let mut summary = call(
            &mut socket,
            "summary",
            "GetBaseReport",
            json!({"requestId": 9, "reportBase": "SummaryInventory"}),
        )
        .await;
        assert_eq!(summary[2]["status"], "Accepted");
        summary = expect_call(&mut socket, "NotifyReport").await;
        let names: Vec<_> = summary["reportData"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["component"]["name"] == "MonitoringCtrlr")
            .map(|entry| entry["variable"]["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["Available", "Enabled"]);
        let selected = json!({"requestId": 10, "componentVariable": [{"component": {"name": "MonitoringCtrlr"}, "variable": {"name": "ItemsPerMessage"}}]});
        send(&mut socket, json!([2, "pick", "GetReport", selected])).await;
        let picked = crate::socket::receive(&mut socket).await;
        assert_eq!(picked[2], json!({"status": "Accepted"}));
        let report = expect_call(&mut socket, "NotifyReport").await;
        let instances: Vec<_> = report["reportData"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["variable"]["instance"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            instances,
            ["SetVariableMonitoring", "ClearVariableMonitoring"]
        );
    };
    let client = run(peer, station(&listener, json!({}))).await;
    client.abort();
}
