use super::http::{Error, Http, Result, bounded};
use reqwest::{Method, Response};
use serde_json::Value;
use std::time::Duration;

#[derive(Default, serde::Serialize)]
pub struct StreamEvidence {
    pub durable: usize,
    pub resumed: bool,
    pub recovered: bool,
    pub checkpoint: Option<String>,
    pub event_types: Vec<String>,
    pub transaction_ids: Vec<String>,
}

struct Frame {
    kind: String,
    id: Option<String>,
    data: String,
}

fn frame(input: &str) -> Result<Option<Frame>> {
    let mut kind = String::new();
    let mut id = None;
    let mut data = String::new();
    for line in input.lines().map(|line| line.trim_end_matches('\r')) {
        if let Some(value) = line.strip_prefix("event:") {
            value.trim_start().clone_into(&mut kind);
        }
        if let Some(value) = line.strip_prefix("id:") {
            id = Some(value.trim_start().to_owned());
        }
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    if kind.is_empty() && data.is_empty() {
        return Ok(None);
    }
    if data.len() > 64 * 1024 {
        return Err(Error("SSE record exceeds bound"));
    }
    Ok(Some(Frame { kind, id, data }))
}

fn recovery(http: &Http, body: &Value) -> Result<(String, String)> {
    if body["recovery"]["action"] != "fetch_fresh_snapshot"
        || body["recovery"]["omit_cursor"] != true
    {
        return Err(Error("invalid cursor recovery"));
    }
    let snapshot = body["recovery"]["snapshot_url"]
        .as_str()
        .ok_or(Error("missing recovery snapshot"))?;
    let subscribe = body["recovery"]["resubscribe_url"]
        .as_str()
        .ok_or(Error("missing recovery subscription"))?;
    http.url(snapshot)?;
    http.url(subscribe)?;
    if http
        .url(subscribe)?
        .query_pairs()
        .any(|(key, _)| key == "after")
    {
        return Err(Error("recovery retained expired cursor"));
    }
    Ok((snapshot.to_owned(), subscribe.to_owned()))
}

async fn open(http: &Http, link: &str, token: &str, checkpoint: Option<&str>) -> Result<Response> {
    let mut url = http.url(link)?;
    if let Some(after) = checkpoint {
        if url.query_pairs().any(|(key, _)| key == "after") {
            return Err(Error("ambiguous SSE cursor"));
        }
        url.query_pairs_mut().append_pair("after", after);
    }
    http.client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| Error("SSE connection failed"))
}

/// The caller retains this response without consuming records during charging. Dropping it
/// disconnects the deliberately stalled subscriber; it has no privileged host access.
pub async fn unread(http: &Http, link: &str, token: &str) -> Result<Response> {
    let response = open(http, link, token, None).await?;
    if response.status().as_u16() != 200 {
        return Err(Error("SSE subscription rejected"));
    }
    Ok(response)
}

struct ReadResult {
    durable: usize,
    recovery: Option<Value>,
    events: Vec<(String, String)>,
}

/// Read a bounded number of complete records; only consumed durable records advance the cursor.
/// A gap or error is terminal even if the server erroneously attaches a durable ID.
async fn consume(
    http: &Http,
    link: &str,
    token: &str,
    checkpoint: &mut Option<String>,
    max_records: usize,
) -> Result<ReadResult> {
    consume_with_window(
        http,
        link,
        token,
        checkpoint,
        max_records,
        Duration::from_secs(4),
        None,
    )
    .await
}

async fn consume_with_window(
    http: &Http,
    link: &str,
    token: &str,
    checkpoint: &mut Option<String>,
    max_records: usize,
    idle_deadline: Duration,
    transaction_id: Option<&str>,
) -> Result<ReadResult> {
    let mut response = open(http, link, token, checkpoint.as_deref()).await?;
    if response.status().as_u16() == 410 {
        let body = serde_json::from_slice::<Value>(&bounded(response, 1024 * 1024).await?)
            .map_err(|_| Error("invalid cursor response"))?;
        return Ok(ReadResult {
            durable: 0,
            recovery: Some(body),
            events: Vec::new(),
        });
    }
    if response.status().as_u16() != 200 {
        return Err(Error("SSE subscription rejected"));
    }
    let mut pending = Vec::new();
    let deadline = tokio::time::Instant::now() + idle_deadline;
    let mut seen = 0;
    let mut durable = 0;
    let mut events = Vec::new();
    let mut total_bytes = 0usize;
    while seen < max_records {
        let chunk = match tokio::time::timeout_at(deadline, response.chunk()).await {
            Ok(Ok(chunk)) => chunk,
            Ok(Err(_)) => return Err(Error("SSE stream failed")),
            Err(_) => break,
        };
        let Some(chunk) = chunk else { break };
        if chunk.len() > 512 * 1024 - total_bytes {
            return Err(Error("SSE page exceeds byte bound"));
        }
        total_bytes += chunk.len();
        if chunk.len() > 64 * 1024 - pending.len() {
            return Err(Error("SSE record exceeds bound"));
        }
        pending.extend_from_slice(&chunk);
        while let Some(end) = pending
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .or_else(|| {
                pending
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .map(|end| end + 2)
            })
        {
            let bytes = pending.drain(..end + 2).collect::<Vec<_>>();
            let text = std::str::from_utf8(&bytes).map_err(|_| Error("invalid SSE encoding"))?;
            let Some(frame) = frame(text)? else { continue };
            seen += 1;
            let body: Value =
                serde_json::from_str(&frame.data).map_err(|_| Error("invalid SSE data"))?;
            match frame.kind.as_str() {
                "durable" => {
                    let id = frame
                        .id
                        .filter(|id| !id.is_empty() && id.len() <= 512)
                        .ok_or(Error("durable SSE record lacks cursor"))?;
                    if body["event_id"].is_null() {
                        return Err(Error("invalid durable SSE envelope"));
                    }
                    let kind = body["event_type"]
                        .as_str()
                        .ok_or(Error("SSE event type missing"))?;
                    let tx = body["payload"]["transaction_id"]
                        .as_str()
                        .ok_or(Error("SSE transaction missing"))?;
                    if transaction_id.is_none_or(|target| tx == target) {
                        events.push((kind.to_owned(), tx.to_owned()));
                    }
                    *checkpoint = Some(id);
                    durable += 1;
                    if transaction_id.is_some_and(|target| tx == target) {
                        return Ok(ReadResult {
                            durable,
                            recovery: None,
                            events,
                        });
                    }
                }
                "gap" => {
                    return Ok(ReadResult {
                        durable,
                        recovery: Some(body),
                        events,
                    });
                }
                "error" => return Err(Error("terminal SSE error")),
                _ => return Err(Error("unknown SSE event")),
            }
            if seen >= max_records {
                break;
            }
        }
    }
    Ok(ReadResult {
        durable,
        recovery: None,
        events,
    })
}

async fn recover(
    http: &Http,
    body: &Value,
    token: &str,
    checkpoint: &mut Option<String>,
) -> Result<String> {
    let (snapshot, subscribe) = recovery(http, body)?;
    let (status, body) = http.request(Method::GET, &snapshot, token, None).await?;
    if status != 200 || body["station"].is_null() {
        return Err(Error("recovery snapshot failed"));
    }
    *checkpoint = None;
    Ok(subscribe)
}

pub async fn expired(http: &Http, link: &str, token: &str) -> Result<bool> {
    // A valid-form cursor that was never issued by this SQLite event journal.
    let mut stale = Some("uob:event:9223372036854775807".to_owned());
    let read = consume(http, link, token, &mut stale, 1).await?;
    let body = read
        .recovery
        .ok_or(Error("stale cursor was not rejected"))?;
    let subscribe = recover(http, &body, token, &mut stale).await?;
    let replay = consume(http, &subscribe, token, &mut stale, 1).await?;
    Ok(replay.recovery.is_none() && replay.durable == 1 && stale.is_some())
}

pub async fn verify_transaction_stream(
    http: &Http,
    link: &str,
    token: &str,
    transaction_id: &str,
) -> Result<(bool, StreamEvidence)> {
    const PAGE_SIZE: usize = 8;
    const MAX_PAGES: usize = 16;
    let scan = async {
        let mut evidence = StreamEvidence::default();
        let mut checkpoint = None;
        let mut subscription = link.to_owned();
        for _ in 0..MAX_PAGES {
            let reconnecting = evidence.durable == 1 && checkpoint.is_some();
            let read = consume_with_window(
                http,
                &subscription,
                token,
                &mut checkpoint,
                PAGE_SIZE,
                Duration::from_millis(300),
                Some(transaction_id),
            )
            .await?;
            if let Some(gap) = read.recovery {
                subscription = recover(http, &gap, token, &mut checkpoint).await?;
                evidence = StreamEvidence {
                    recovered: true,
                    ..StreamEvidence::default()
                };
                continue;
            }
            subscription = link.to_owned();
            for (kind, tx) in read.events {
                let expected = if evidence.durable == 0 {
                    "transaction.started"
                } else {
                    "transaction.ended"
                };
                if kind != expected {
                    return Err(Error("durable SSE transaction correlation incomplete"));
                }
                evidence.event_types.push(kind);
                evidence.transaction_ids.push(tx);
                evidence.durable += 1;
                if evidence.durable == 2 {
                    evidence.resumed = reconnecting;
                    if !evidence.resumed {
                        return Err(Error("durable SSE transaction correlation incomplete"));
                    }
                    evidence.checkpoint = checkpoint.clone();
                    let expired_cursor_recovered = expired(http, link, token).await?;
                    if !expired_cursor_recovered {
                        return Err(Error("expired cursor not recovered"));
                    }
                    return Ok((expired_cursor_recovered, evidence));
                }
            }
        }
        Err(Error("durable SSE transaction correlation incomplete"))
    };
    tokio::time::timeout(Duration::from_secs(8), scan)
        .await
        .map_err(|_| Error("SSE transaction scan deadline exceeded"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Query, State},
        http::StatusCode,
        response::IntoResponse,
        routing::get,
    };
    use serde_json::json;
    use std::{
        collections::HashMap,
        fmt::Write as _,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[tokio::test]
    async fn historical_transactions_are_skipped_and_current_pair_reconnects_on_same_resource() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let reconnects = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/bridge/v1/stations/station-a",
                get(|| async { Json(json!({"station":{"station_id":"station-a"}})) }),
            )
            .route(
                "/bridge/v1/events",
                get(
                    |State((base, reconnects)): State<(String, Arc<AtomicUsize>)>,
                     Query(params): Query<HashMap<String, String>>| async move {
                        if params.get("station_id").map(String::as_str) != Some("station-a")
                            || params.get("evse_id").map(String::as_str) != Some("evse-1")
                        {
                            return StatusCode::BAD_REQUEST.into_response();
                        }
                        let cursor = params.get("after");
                        if cursor.is_some_and(|id| id == "uob:event:2") {
                            reconnects.fetch_or(1, Ordering::SeqCst);
                        } else if cursor.is_some_and(|id| id == "uob:event:3") {
                            reconnects.fetch_or(2, Ordering::SeqCst);
                        }
                        if cursor.is_some_and(|id| id == "uob:event:9223372036854775807") {
                            return (
                                StatusCode::GONE,
                                Json(json!({"recovery":{
                                    "action":"fetch_fresh_snapshot",
                                    "omit_cursor":true,
                                    "snapshot_url":format!("{base}/bridge/v1/stations/station-a"),
                                    "resubscribe_url":format!("{base}/bridge/v1/events?station_id=station-a&evse_id=evse-1")
                                }})),
                            )
                                .into_response();
                        }
                        let after = cursor
                            .and_then(|cursor| cursor.strip_prefix("uob:event:"))
                            .and_then(|index| index.parse::<usize>().ok())
                            .unwrap_or(0);
                        let available = if after == 0 { 2 } else { 4 };
                        let mut events = String::new();
                        for index in after + 1..=available {
                            let (kind, transaction) = match index {
                                1 => ("transaction.started", "previous"),
                                2 => ("transaction.ended", "previous"),
                                3 => ("transaction.started", "current"),
                                4 => ("transaction.ended", "current"),
                                _ => unreachable!(),
                            };
                            let body = json!({"event_id":format!("event-{index}"),
                                "event_type":kind,"payload":{"transaction_id":transaction}});
                            write!(
                                events,
                                "event: durable\nid: uob:event:{index}\ndata: {body}\n\n"
                            )
                            .unwrap();
                        }
                        (StatusCode::OK, [("content-type", "text/event-stream")], events)
                            .into_response()
                    },
                ),
            )
            .with_state((base.clone(), reconnects.clone()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = Http::new(&base).unwrap();
        let path = "/bridge/v1/events?station_id=station-a&evse_id=evse-1";
        let (recovered, evidence) = verify_transaction_stream(&http, path, "test-token", "current")
            .await
            .unwrap();
        assert!(recovered && evidence.resumed);
        assert_eq!(evidence.durable, 2);
        assert_eq!(
            evidence.event_types,
            ["transaction.started", "transaction.ended"]
        );
        assert_eq!(evidence.transaction_ids, ["current", "current"]);
        assert_eq!(evidence.checkpoint.as_deref(), Some("uob:event:4"));
        assert_eq!(reconnects.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn terminal_gap_discards_control_id_and_resumes_from_fresh_snapshot() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/bridge/v1/stations/station-a", get(|| async {
                Json(json!({"station":{"station_id":"station-a"}}))
            }))
            .route("/bridge/v1/events", get(|State(base): State<String>,
                Query(params): Query<HashMap<String, String>>| async move {
                let data = if params.get("after").is_some_and(|value| value == "uob:event:1") {
                    "event: durable\nid: uob:event:2\ndata: {\"event_id\":\"second\",\"event_type\":\"transaction.ended\",\"payload\":{\"transaction_id\":\"tx-1\"}}\n\n".to_owned()
                } else if params.contains_key("fresh") {
                    "event: durable\nid: uob:event:1\ndata: {\"event_id\":\"first\",\"event_type\":\"transaction.started\",\"payload\":{\"transaction_id\":\"tx-1\"}}\n\n".to_owned()
                } else {
                    format!("event: gap\nid: uob:event:999\ndata: {{\"recovery\":{{\"action\":\"fetch_fresh_snapshot\",\"omit_cursor\":true,\"snapshot_url\":\"{base}/bridge/v1/stations/station-a\",\"resubscribe_url\":\"{base}/bridge/v1/events?fresh=1\"}}}}\n\n")
                };
                ([("content-type", "text/event-stream")], data)
            }))
            .with_state(base.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = Http::new(&base).unwrap();
        let (recovered, evidence) =
            verify_transaction_stream(&http, "/bridge/v1/events", "test-token", "tx-1")
                .await
                .unwrap();
        assert_eq!(evidence.durable, 2);
        assert!(recovered && evidence.recovered && evidence.resumed);
        assert_eq!(evidence.checkpoint.as_deref(), Some("uob:event:2"));
        assert_eq!(
            evidence.event_types,
            ["transaction.started", "transaction.ended"]
        );
        assert_eq!(evidence.transaction_ids, ["tx-1", "tx-1"]);
        server.abort();
    }

    #[test]
    fn only_same_origin_uncredentialed_recovery_links_are_safe() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let http = Http::new("https://bridge.test/").unwrap();
        let body = json!({"recovery":{"action":"fetch_fresh_snapshot","omit_cursor":true,
            "snapshot_url":"https://other.test/bridge/v1/stations/station-a",
            "resubscribe_url":"/bridge/v1/events"}});
        assert!(recovery(&http, &body).is_err());
        let mut body = body;
        body["recovery"]["snapshot_url"] =
            json!("https://user:pass@bridge.test/bridge/v1/stations/station-a");
        assert!(recovery(&http, &body).is_err());
    }
}
