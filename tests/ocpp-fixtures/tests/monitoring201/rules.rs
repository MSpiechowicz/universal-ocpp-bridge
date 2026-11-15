//! Independent reference rules for the CSMS-side native checks above the schema floor.

use serde_json::Value;

pub fn payload(frame: &Value) -> &Value {
    if frame[0] == 2 { &frame[3] } else { &frame[2] }
}

pub fn action(frame: &Value) -> &str {
    frame[2].as_str().unwrap_or("")
}

fn walk<'a>(key: &str, value: &'a Value, visit: &mut impl FnMut(&str, &'a Value)) {
    match value {
        Value::Object(map) => {
            for (child_key, child) in map {
                visit(child_key, child);
                walk(child_key, child, visit);
            }
        }
        Value::Array(items) => {
            for item in items {
                // Array items inherit the name of the field that holds the array.
                if !item.is_object() && !item.is_array() {
                    visit(key, item);
                }
                walk(key, item, visit);
            }
        }
        _ => {}
    }
}

fn component_variable(item: &Value) -> (Value, Value) {
    (item["component"].clone(), item["variable"].clone())
}

fn ids(value: &Value) -> Vec<i64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_i64().unwrap())
        .collect()
}

/// Single-frame native rules: they hold for every frame the bridge sends or accepts.
pub fn single_frame_refusal(frame: &Value) -> Vec<&'static str> {
    let body = payload(frame);
    let mut refused = Vec::new();
    walk("", body, &mut |key, value| {
        if key == "severity" && !value.as_i64().is_some_and(|v| (0..=9).contains(&v)) {
            refused.push("severity-range");
        }
        if let Some(number) = value.as_i64()
            && !matches!(key, "value")
            && i32::try_from(number).is_err()
        {
            refused.push("int32");
        }
        if let Some(text) = value.as_str()
            && text.chars().any(char::is_control)
        {
            refused.push("control-chars");
        }
    });
    if action(frame) == "SetVariableMonitoring" {
        for item in body["setMonitoringData"].as_array().unwrap() {
            if item["type"] == "Delta" && item["value"].as_f64().unwrap() < 0.0 {
                refused.push("delta-negative");
            }
        }
    }
    if action(frame) == "NotifyEvent" {
        for event in body["eventData"].as_array().unwrap() {
            if event["eventNotificationType"] != "HardWiredNotification"
                && event.get("variableMonitoringId").is_none()
            {
                refused.push("event-monitor-id");
            }
        }
    }
    refused
}

fn monitor_criterion(kind: &str) -> &'static str {
    match kind {
        "UpperThreshold" | "LowerThreshold" => "ThresholdMonitoring",
        "Delta" => "DeltaMonitoring",
        _ => "PeriodicMonitoring",
    }
}

/// Rules that relate a reply or fragment to the frames before it.
pub fn contextual_refusal(rule: &str, context: &[Value], frame: &Value) -> bool {
    let body = payload(frame);
    let request = context.first().map(payload);
    match rule {
        "set-count" => {
            request.unwrap()["setMonitoringData"]
                .as_array()
                .unwrap()
                .len()
                != body["setMonitoringResult"].as_array().unwrap().len()
        }
        "set-identity" => {
            let requested = request.unwrap()["setMonitoringData"].as_array().unwrap();
            body["setMonitoringResult"]
                .as_array()
                .unwrap()
                .iter()
                .any(|result| {
                    !requested
                        .iter()
                        .any(|item| component_variable(item) == component_variable(result))
                })
        }
        "set-accepted-id" => body["setMonitoringResult"]
            .as_array()
            .unwrap()
            .iter()
            .any(|result| result["status"] == "Accepted" && result.get("id").is_none()),
        "set-id-echo" => {
            let requested = request.unwrap()["setMonitoringData"].as_array().unwrap();
            requested
                .iter()
                .zip(body["setMonitoringResult"].as_array().unwrap())
                .any(|(item, result)| {
                    item.get("id").is_some()
                        && result["status"] == "Accepted"
                        && item["id"] != result["id"]
                })
        }
        "clear-ids" => {
            let mut wanted = ids(&request.unwrap()["id"]);
            let mut got: Vec<i64> = body["clearMonitoringResult"]
                .as_array()
                .unwrap()
                .iter()
                .map(|result| result["id"].as_i64().unwrap())
                .collect();
            wanted.sort_unstable();
            got.sort_unstable();
            wanted != got
        }
        "fragment-sequence" => {
            let parts: Vec<&Value> = context
                .iter()
                .filter(|earlier| action(earlier) == action(frame))
                .collect();
            let expected = i64::try_from(parts.len()).unwrap();
            let previous_open = parts
                .last()
                .is_none_or(|earlier| payload(earlier)["tbc"] == true);
            body["seqNo"].as_i64().unwrap() != expected || !previous_open
        }
        "report-request-id" => context
            .iter()
            .find(|earlier| action(earlier) == "GetMonitoringReport")
            .is_none_or(|get| payload(get)["requestId"] != body["requestId"]),
        "report-criteria" => {
            let criteria = request.unwrap()["monitoringCriteria"].as_array().unwrap();
            body["monitor"].as_array().unwrap().iter().any(|data| {
                data["variableMonitoring"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|monitor| {
                        !criteria
                            .iter()
                            .any(|c| c == monitor_criterion(monitor["type"].as_str().unwrap()))
                    })
            })
        }
        "report-filter" => {
            let filter = request.unwrap()["componentVariable"].as_array().unwrap();
            body["monitor"].as_array().unwrap().iter().any(|data| {
                !filter.iter().any(|wanted| {
                    wanted["component"]["name"] == data["component"]["name"]
                        && wanted
                            .get("variable")
                            .is_none_or(|variable| variable["name"] == data["variable"]["name"])
                })
            })
        }
        other => panic!("unknown contextual rule {other}"),
    }
}
