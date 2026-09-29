use serde_json::Value;

use crate::{
    Error, Result,
    config::{Config, Station},
};

pub fn resource(snapshot: &Value, config: &Config, station: &Station) -> Result<Value> {
    if snapshot["station"]["bridge_id"] != config.bridge_id
        || snapshot["station"]["station_id"] != station.id
        || snapshot["connectivity"]["status"] != "connected"
    {
        return Err(Error("station identity or connectivity mismatch"));
    }
    if !snapshot["capabilities"]["operations"]
        .as_array()
        .is_some_and(|ops| {
            ops.iter().any(|op| op["operation"]["kind"] == "start")
                && ops.iter().any(|op| op["operation"]["kind"] == "stop")
        })
    {
        return Err(Error("station does not advertise start and stop"));
    }
    snapshot["resources"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|item| {
                let resource = &item["resource"];
                resource["bridge_id"] == config.bridge_id
                    && resource["station_id"] == station.id
                    && resource["native_protocol_reference"]["protocol"] == station.protocol
                    && resource["resource"]["kind"]
                        == if station.protocol == "ocpp16" {
                            "connector"
                        } else {
                            "evse"
                        }
            })
        })
        .ok_or(Error("station has no OCPP charging resource"))?;
    Ok(snapshot["station"].clone())
}

fn transactions(snapshot: &Value) -> Result<&[Value]> {
    match snapshot.get("transactions") {
        None => Ok(&[]),
        Some(Value::Array(transactions)) => Ok(transactions),
        Some(_) => Err(Error("invalid transactions")),
    }
}

pub fn baseline(snapshot: &Value) -> Result<Vec<String>> {
    Ok(transactions(snapshot)?
        .iter()
        .filter_map(|tx| tx["transaction_id"].as_str().map(str::to_owned))
        .collect())
}

pub fn effect(
    snapshot: &Value,
    config: &Config,
    station: &Station,
    resource: &Value,
    previous: &[String],
    started: Option<&str>,
) -> Result<Option<String>> {
    if snapshot["station"]["bridge_id"] != config.bridge_id
        || snapshot["station"]["station_id"] != station.id
    {
        return Err(Error("effect snapshot belongs to another station"));
    }
    let transactions = transactions(snapshot)?;
    for tx in transactions {
        let Some(id) = tx["transaction_id"].as_str() else {
            continue;
        };
        if tx["resource"]["bridge_id"] != resource["bridge_id"]
            || tx["resource"]["station_id"] != resource["station_id"]
        {
            continue;
        }
        if let Some(started) = started {
            if id == started && tx["state"] == "ended" {
                return Ok(Some(id.to_owned()));
            }
        } else if !previous.iter().any(|old| old == id)
            && (tx["state"] == "active"
                || (station.protocol == "ocpp16"
                    && tx["state"] == "pending"
                    && tx["ocpp16"]["start_message_id"].as_str().is_some()))
        {
            return Ok(Some(id.to_owned()));
        }
    }
    Ok(None)
}

pub fn accepted(
    result: &Value,
    config: &Config,
    station: &Station,
    resource: &Value,
    request_id: &str,
    correlation: Option<&str>,
) -> Result<bool> {
    if result["return_route"]["request_id"] != request_id
        || result["return_route"]["origin"]["kind"] != "target"
        || result["return_route"]["origin"]["target_instance_id"] != config.target_instance_id
        || &result["resource"] != resource
        || result["resource"]["station_id"] != station.id
        || correlation.is_some_and(|value| result["correlation_id"] != value)
    {
        return Err(Error("command result identity mismatch"));
    }
    match result["lifecycle"]["stage"].as_str() {
        Some("protocol_response") if result["lifecycle"]["accepted"] == true => Ok(true),
        Some("protocol_response") => Err(Error("native protocol refused charging command")),
        Some("rejected") => Err(Error("charging command rejected")),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn charging_evidence_never_counts_old_or_unrelated_transactions() {
        let config = Config {
            bridge_id: "site".into(),
            environment: "demo".into(),
            target_instance_id: "main".into(),
            station: vec![
                Station {
                    id: "s1".into(),
                    protocol: "ocpp16".into(),
                    authorization_reference: "demo".into(),
                },
                Station {
                    id: "s2".into(),
                    protocol: "ocpp201".into(),
                    authorization_reference: "demo".into(),
                },
            ],
        };
        let station = &config.station[0];
        let resource = json!({"bridge_id":"site","station_id":"s1","resource":{"kind":"connector","connector_id":"1"}});
        let empty = json!({"station":{"bridge_id":"site","station_id":"s1"}});
        assert!(baseline(&empty).unwrap().is_empty());
        assert_eq!(
            effect(&empty, &config, station, &resource, &[], None).unwrap(),
            None
        );
        assert!(baseline(&json!({"transactions":null})).is_err());
        let pending = json!({"station":{"bridge_id":"site","station_id":"s1"},"transactions":[
            {"transaction_id":"ocpp16/1","resource":resource,"state":"pending",
             "ocpp16":{"start_message_id":"native-start"}}]});
        assert_eq!(
            effect(&pending, &config, station, &resource, &[], None).unwrap(),
            Some("ocpp16/1".into())
        );
        let unproven = json!({"station":{"bridge_id":"site","station_id":"s1"},"transactions":[
            {"transaction_id":"ocpp16/1","resource":resource,"state":"pending"}]});
        assert_eq!(
            effect(&unproven, &config, station, &resource, &[], None).unwrap(),
            None
        );
        let base = json!({"station":{"bridge_id":"site","station_id":"s1"},"transactions":[
            {"transaction_id":"old","resource":resource,"state":"active"}]});
        assert_eq!(
            effect(&base, &config, station, &resource, &["old".into()], None).unwrap(),
            None
        );
        let wrong = json!({"station":{"bridge_id":"site","station_id":"s1"},"transactions":[
            {"transaction_id":"new","resource":{"bridge_id":"site","station_id":"s2"},"state":"active"}]});
        assert_eq!(
            effect(&wrong, &config, station, &resource, &["old".into()], None).unwrap(),
            None
        );
        let started = json!({"station":{"bridge_id":"site","station_id":"s1"},"transactions":[
            {"transaction_id":"new","resource":resource,"state":"active"}]});
        assert_eq!(
            effect(&started, &config, station, &resource, &["old".into()], None).unwrap(),
            Some("new".into())
        );
        assert_eq!(
            effect(&started, &config, station, &resource, &[], Some("new")).unwrap(),
            None
        );
        let ended = json!({"station":{"bridge_id":"site","station_id":"s1"},"transactions":[
            {"transaction_id":"new","resource":resource,"state":"ended"}]});
        assert_eq!(
            effect(&ended, &config, station, &resource, &[], Some("new")).unwrap(),
            Some("new".into())
        );
        assert_eq!(
            effect(&ended, &config, station, &resource, &[], Some("other")).unwrap(),
            None
        );
    }
    #[test]
    fn result_cannot_substitute_admission_or_another_targets_protocol_reply() {
        let config = Config {
            bridge_id: "site".into(),
            environment: "demo".into(),
            target_instance_id: "main".into(),
            station: vec![
                Station {
                    id: "s1".into(),
                    protocol: "ocpp16".into(),
                    authorization_reference: "ref".into(),
                },
                Station {
                    id: "s2".into(),
                    protocol: "ocpp201".into(),
                    authorization_reference: "ref".into(),
                },
            ],
        };
        let resource = json!({"bridge_id":"site","station_id":"s1","resource":null});
        let mut result = json!({"resource":resource,
            "return_route":{"request_id":"new","origin":{"kind":"target","target_instance_id":"main"}},
            "correlation_id":"new","lifecycle":{"stage":"admitted"}});
        let station = &config.station[0];
        assert!(!accepted(&result, &config, station, &resource, "new", Some("new")).unwrap());
        result["lifecycle"] = json!({"stage":"protocol_response","accepted":true});
        assert!(accepted(&result, &config, station, &resource, "new", Some("new")).unwrap());
        result["return_route"]["origin"]["target_instance_id"] = json!("other");
        assert!(accepted(&result, &config, station, &resource, "new", Some("new")).is_err());
        result["return_route"]["origin"]["target_instance_id"] = json!("main");
        result["lifecycle"]["accepted"] = json!(false);
        assert!(accepted(&result, &config, station, &resource, "new", Some("new")).is_err());
    }
}
