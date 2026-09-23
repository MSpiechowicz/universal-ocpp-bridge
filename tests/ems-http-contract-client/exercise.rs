use super::{
    Demo, Scenario,
    http::{Error, Http, Result},
    sse,
};
use queries::{accepted_status, event_path, page_walk, point_matches, station_matches, text};
use reqwest::Method;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Serialize)]
struct InventoryEvidence {
    stations: usize,
    points: usize,
    point_value: bool,
}

#[derive(Serialize)]
struct AccessEvidence {
    reader_denied: bool,
    unauthorized_data_denied: bool,
    expired_rejected: bool,
}

#[derive(Serialize)]
struct CommandEvidence {
    admission_http_status: u16,
    protocol_accepted: bool,
    observed_effects: usize,
    deduplicated: bool,
}

#[derive(Serialize)]
struct SubscriptionEvidence {
    subscription_mode: &'static str,
    expired_cursor_recovered: bool,
}

#[derive(Serialize)]
pub struct ScenarioEvidence {
    protocol: String,
    #[serde(flatten)]
    inventory: InventoryEvidence,
    #[serde(flatten)]
    access: AccessEvidence,
    #[serde(flatten)]
    commands: CommandEvidence,
    #[serde(flatten)]
    subscription: SubscriptionEvidence,
    sse: sse::StreamEvidence,
}

#[derive(Serialize)]
pub struct Evidence {
    pub status: &'static str,
    pub scenarios: Vec<ScenarioEvidence>,
}

async fn command(http: &Http, token: &str, payload: &Value) -> Result<(u16, Value)> {
    http.request(Method::POST, "/bridge/v1/commands", token, Some(payload))
        .await
}

async fn inventory(
    http: &Http,
    station_path: &str,
    station: &str,
    protocol: &str,
    operator: &str,
) -> Result<(InventoryEvidence, Value)> {
    http.url(station_path)?;
    let mut snapshot = None;
    for _ in 0..50 {
        let (status, body) = http
            .request(Method::GET, station_path, operator, None)
            .await?;
        if status == 200 && station_matches(&body, protocol) {
            snapshot = Some(body);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let snapshot = snapshot.ok_or(Error("station protocol not observed"))?;
    let stations = page_walk(http, "/bridge/v1/stations?limit=1", operator, None, 1).await?;
    if stations < 2 {
        return Err(Error("station pagination incomplete"));
    }
    let point_path = format!("/bridge/v1/points?station_id={station}&limit=2");
    let points = page_walk(http, &point_path, operator, Some(station), 2).await?;
    if points <= 2 {
        return Err(Error("station point pagination incomplete"));
    }
    let resource_item = snapshot["resources"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or(Error("missing command resource"))?;
    let resource = resource_item["resource"].clone();
    let point_id = resource_item["current_values"]
        .as_array()
        .and_then(|values| values.first())
        .and_then(|value| value["point_id"].as_str())
        .ok_or(Error("missing point value"))?;
    let mut point_url = http.url(&format!("/bridge/v1/points/{point_id}"))?;
    point_url
        .query_pairs_mut()
        .append_pair("station_id", station);
    let item = &resource["resource"];
    for key in ["evse_id", "connector_id"] {
        if let Some(value) = item[key].as_str() {
            point_url.query_pairs_mut().append_pair(key, value);
        }
    }
    let (point_status, point_body) = http
        .request(Method::GET, point_url.as_str(), operator, None)
        .await?;
    let point_value = point_status == 200 && point_matches(&point_body, point_id);
    if !point_value {
        return Err(Error("point value unavailable"));
    }
    Ok((
        InventoryEvidence {
            stations,
            points,
            point_value,
        },
        resource,
    ))
}

async fn access_checks(
    http: &Http,
    reader: &str,
    operator: &str,
    scenario: &Scenario,
    station_path: &str,
    denied_station: &str,
    resource: &Value,
) -> Result<AccessEvidence> {
    let station = scenario
        .command_station
        .as_deref()
        .ok_or(Error("station missing"))?;
    let authorization_reference = scenario
        .authorization_reference
        .as_deref()
        .ok_or(Error("authorization reference missing"))?;
    let reader_denied = command(http, reader, &json!({
        "request_id": format!("reader-denied-{station}"),
        "resource": resource,
        "operation":{"kind":"start","parameters":{"authorization_reference":authorization_reference}},
        "expires_at":"2099-01-01T00:00:00Z"
    })).await?.0 == 403;
    let (status, _) = http
        .request(
            Method::GET,
            &format!("/bridge/v1/stations/{denied_station}"),
            reader,
            None,
        )
        .await?;
    let unauthorized_data_denied = status == 403;
    if !scenario.reader_out_of_scope {
        let (status, _) = http
            .request(Method::GET, station_path, reader, None)
            .await?;
        if status != 200 {
            return Err(Error("authorized reader data unavailable"));
        }
    }
    if !reader_denied || !unauthorized_data_denied {
        return Err(Error("reader scope violation"));
    }
    let expired = json!({"request_id":format!("expired-{station}"),
        "resource":resource, "operation":{"kind":"start","parameters":{"authorization_reference":authorization_reference}},
        "expires_at":"2000-01-01T00:00:00Z"});
    let (expired_status, _) = command(http, operator, &expired).await?;
    let expired_rejected = expired_status == 410;
    if !expired_rejected {
        return Err(Error("expired command admitted"));
    }
    Ok(AccessEvidence {
        reader_denied,
        unauthorized_data_denied,
        expired_rejected,
    })
}

async fn charging_commands(
    http: &Http,
    station: &str,
    station_path: &str,
    operator: &str,
    resource: &Value,
    authorization_reference: &str,
    invocation_id: &str,
) -> Result<(CommandEvidence, Value)> {
    let mut protocol_accepted = true;
    let mut observed_effects = 0;
    let mut deduplicated = true;
    let mut admission_http_status = 0;
    let mut observed_transaction: Option<Value> = None;
    for kind in ["start", "stop"] {
        let request_id = format!("http-{invocation_id}-{kind}-{station}");
        let parameters = if kind == "start" {
            json!({"authorization_reference":authorization_reference})
        } else {
            json!({"transaction_id":text(observed_transaction.as_ref()
                .ok_or(Error("station start effect not observed"))?, "transaction_id")?})
        };
        let payload = json!({"request_id":request_id,
            "resource":resource, "operation":{"kind":kind,"parameters":parameters},
            "expires_at":"2099-01-01T00:00:00Z"});
        let (status, admitted) = command(http, operator, &payload).await?;
        admission_http_status = status;
        if status != 202 || admitted["request_id"] != request_id {
            return Err(Error("command not durably admitted"));
        }
        let status_url = text(&admitted, "status_url")?;
        let (retry_status, retry) = command(http, operator, &payload).await?;
        deduplicated &= retry_status == 202
            && retry["request_id"] == request_id
            && retry["status_url"] == admitted["status_url"];
        let mut conflicting = payload.clone();
        if kind == "start" {
            conflicting["operation"]["parameters"]["authorization_reference"] = json!("different");
        } else {
            conflicting["operation"]["parameters"]["transaction_id"] = json!("different");
        }
        // Reusing a request ID with a different payload must not dispatch a second command.
        let (conflict_status, _) = command(http, operator, &conflicting).await?;
        if conflict_status != 409 {
            return Err(Error("conflicting duplicate admitted"));
        }
        let status_url = status_url.to_owned();
        protocol_accepted &= accepted_status(http, &status_url, operator, &request_id).await?;
        let transaction_id = observed_transaction
            .as_ref()
            .and_then(|tx| tx["transaction_id"].as_str());
        let observed = queries::transaction(
            http,
            station_path,
            operator,
            transaction_id,
            if kind == "start" { "pending" } else { "ended" },
        )
        .await?;
        if observed["resource"]["station_id"] != station {
            return Err(Error("transaction belongs to another station"));
        }
        if kind == "stop"
            && observed_transaction
                .as_ref()
                .is_some_and(|started| observed["transaction_id"] != started["transaction_id"])
        {
            return Err(Error("stop did not end started transaction"));
        }
        observed_transaction = Some(observed);
        observed_effects += 1;
    }
    if !deduplicated || !protocol_accepted || observed_effects != 2 {
        return Err(Error("command and station outcome mismatch"));
    }
    Ok((
        CommandEvidence {
            admission_http_status,
            protocol_accepted,
            observed_effects,
            deduplicated,
        },
        observed_transaction.ok_or(Error("station effect missing"))?,
    ))
}

/// External HTTP/SSE-only client. Charger-side actions are orchestrated by the test host;
/// these fields never infer physical effects from HTTP admission alone.
pub async fn run(
    http_base: &str,
    reader: &str,
    operator: &str,
    demo: &Demo,
    allow_remote: bool,
) -> Result<Evidence> {
    run_with_deadline(
        http_base,
        reader,
        operator,
        demo,
        allow_remote,
        Duration::from_secs(45),
    )
    .await
}

async fn run_with_deadline(
    http_base: &str,
    reader: &str,
    operator: &str,
    demo: &Demo,
    allow_remote: bool,
    deadline: Duration,
) -> Result<Evidence> {
    tokio::time::timeout(
        deadline,
        run_operation(http_base, reader, operator, demo, allow_remote),
    )
    .await
    .map_err(|_| Error("exercise deadline exceeded"))?
}

async fn run_operation(
    base: &str,
    reader: &str,
    operator: &str,
    demo: &Demo,
    allow_remote: bool,
) -> Result<Evidence> {
    let http = Http::new_for_exercise(base, allow_remote)?;
    safety::require_protocol_coverage(demo)?;
    let invocation_id = uuid::Uuid::new_v4().to_string();
    let mut scenarios = Vec::new();
    let (status, caps) = http
        .request(Method::GET, "/bridge/v1/capabilities", reader, None)
        .await?;
    if status != 200 || caps["target"]["kind"] != "ems-scada.http" {
        return Err(Error("invalid capabilities"));
    }
    let denied_station = demo
        .scenario
        .iter()
        .find(|scenario| scenario.reader_out_of_scope)
        .and_then(|scenario| scenario.command_station.as_deref())
        .ok_or(Error("out-of-scope station missing"))?;
    for scenario in &demo.scenario {
        let Some(station) = scenario.command_station.as_deref() else {
            continue;
        };
        let authorization_reference = scenario
            .authorization_reference
            .as_deref()
            .ok_or(Error("authorization reference missing"))?;
        let station_path = format!("/bridge/v1/stations/{station}");
        let (inventory, resource) =
            inventory(&http, &station_path, station, &scenario.protocol, operator).await?;
        let access = access_checks(
            &http,
            reader,
            operator,
            scenario,
            &station_path,
            denied_station,
            &resource,
        )
        .await?;
        let subscription_mode = if scenario.protocol == "ocpp16" {
            "absent"
        } else {
            "stalled"
        };
        let stalled = if subscription_mode == "stalled" {
            let mut station_events = http.url("/bridge/v1/events")?;
            station_events
                .query_pairs_mut()
                .append_pair("station_id", station);
            Some(sse::unread(&http, station_events.as_str(), operator).await?)
        } else {
            None
        };
        if stalled.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let (commands, transaction) = charging_commands(
            &http,
            station,
            &station_path,
            operator,
            &resource,
            authorization_reference,
            &invocation_id,
        )
        .await?;
        drop(stalled);
        let path = event_path(&http, station, &transaction["resource"])?;
        let observed_id = text(&transaction, "transaction_id")?;
        let (expired_cursor_recovered, sse) =
            sse::verify_transaction_stream(&http, &path, operator, observed_id).await?;
        let subscription = SubscriptionEvidence {
            subscription_mode,
            expired_cursor_recovered,
        };
        scenarios.push(ScenarioEvidence {
            protocol: scenario.protocol.clone(),
            inventory,
            access,
            commands,
            subscription,
            sse,
        });
    }
    Ok(Evidence {
        status: "passed",
        scenarios,
    })
}

#[path = "exercise_safety.rs"]
mod safety;

#[path = "exercise_queries.rs"]
mod queries;

#[cfg(test)]
#[path = "exercise_tests.rs"]
mod tests;
