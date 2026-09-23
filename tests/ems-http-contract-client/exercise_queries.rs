use super::{Error, Http, Method, Result, Value};
use std::collections::BTreeSet;

pub(super) fn text<'a>(body: &'a Value, key: &str) -> Result<&'a str> {
    body[key].as_str().ok_or(Error("missing response field"))
}
pub(super) fn station_matches(body: &Value, protocol: &str) -> bool {
    body["resources"].as_array().is_some_and(|resources| {
        resources
            .iter()
            .any(|item| item["resource"]["native_protocol_reference"]["protocol"] == protocol)
    }) && body["connectivity"]["status"] == "connected"
}

pub(super) fn point_matches(body: &Value, point_id: &str) -> bool {
    body["point_id"] == point_id && !body["value"]["quality"].is_null()
}

pub(super) fn event_path(http: &Http, station: &str, resource: &Value) -> Result<String> {
    let mut url = http.url("/bridge/v1/events")?;
    url.query_pairs_mut().append_pair("station_id", station);
    let item = &resource["resource"];
    for key in ["evse_id", "connector_id"] {
        if let Some(value) = item[key].as_str() {
            url.query_pairs_mut().append_pair(key, value);
        }
    }
    Ok(url.to_string())
}

pub(super) async fn accepted_status(
    http: &Http,
    status_url: &str,
    token: &str,
    request_id: &str,
) -> Result<bool> {
    for _ in 0..50 {
        let (status, result) = http.request(Method::GET, status_url, token, None).await?;
        if status != 200 || result["return_route"]["request_id"] != request_id {
            return Err(Error("command status unavailable"));
        }
        if result["lifecycle"]["stage"] == "protocol_response" {
            return Ok(result["lifecycle"]["accepted"] == true);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(Error("protocol response not observed"))
}

pub(super) async fn page_walk(
    http: &Http,
    path: &str,
    token: &str,
    station: Option<&str>,
    per_page: usize,
) -> Result<usize> {
    let mut link = path.to_owned();
    let mut cursors = BTreeSet::new();
    let mut items = 0;
    for _ in 0..64 {
        let (status, body) = http.request(Method::GET, &link, token, None).await?;
        if status != 200 {
            return Err(Error("inventory query rejected"));
        }
        let page = body["items"]
            .as_array()
            .ok_or(Error("invalid inventory page"))?;
        if page.len() > per_page {
            return Err(Error("inventory exceeded requested page limit"));
        }
        for item in page {
            if let Some(station) = station
                && item["resource"]["station_id"] != station
                && item["station"]["station_id"] != station
            {
                return Err(Error("filter returned another station"));
            }
            items += 1;
        }
        let Some(next) = body["next_cursor"].as_str() else {
            return Ok(items);
        };
        if !cursors.insert(next.to_owned()) {
            return Err(Error("repeated page cursor"));
        }
        let mut url = http.url(path)?;
        url.query_pairs_mut().append_pair("after", next);
        url.as_str().clone_into(&mut link);
    }
    Err(Error("page walk exceeded bound"))
}

pub(super) async fn transaction(
    http: &Http,
    station_path: &str,
    operator: &str,
    id: Option<&str>,
    state: &str,
) -> Result<Value> {
    for _ in 0..50 {
        let (status, snapshot) = http
            .request(Method::GET, station_path, operator, None)
            .await?;
        if status != 200 {
            return Err(Error("transaction snapshot unavailable"));
        }
        if let Some(found) = snapshot["transactions"]
            .as_array()
            .and_then(|transactions| {
                transactions.iter().find(|tx| {
                    tx["state"] == state && id.is_none_or(|id| tx["transaction_id"] == id)
                })
            })
        {
            return Ok(found.clone());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(Error("station transaction effect not observed"))
}
