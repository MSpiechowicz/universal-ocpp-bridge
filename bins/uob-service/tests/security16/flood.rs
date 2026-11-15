use super::support::*;

/// A distinct event per index, one second apart.
fn distinct(prefix: &str, index: u32) -> serde_json::Value {
    event_call(
        &format!("{prefix}-{index}"),
        "InvalidMessages",
        &format!("2026-10-07T07:{:02}:{:02}Z", index / 60, index % 60),
        None,
    )
}

#[tokio::test]
async fn a_flood_is_acknowledged_shed_and_signalled_without_dropping_the_session() {
    let (fixture, _process) = launch(Options::default()).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let before = dropped_diagnostics(&client, &fixture).await;

    // The per-station budget is 60 records per minute. Every CALL still gets the empty reply.
    for index in 0..100 {
        notify(&mut socket, distinct("flood", index)).await;
    }
    assert_eq!(stored(&client, &fixture).await, 60);
    assert_eq!(dropped_diagnostics(&client, &fixture).await - before, 40);
    let mut journal = Journal::open(&client, &fixture).await;
    for _ in 0..60 {
        let event = journal.next().await;
        assert!(record(&event).get("shed_calls").is_none());
    }

    // The session survives the flood and keeps serving.
    let heartbeat = station_call(&mut socket, "beat", "Heartbeat", serde_json::json!({})).await;
    assert!(heartbeat["currentTime"].is_string());

    // The budget belongs to the station: a reconnect neither refreshes it nor forgets what was
    // shed. New events are still acknowledged, none is stored, each one is counted.
    let shed = dropped_diagnostics(&client, &fixture).await;
    drop(socket);
    let mut socket = connected(&fixture, &client).await;
    for index in 100..103 {
        notify(&mut socket, distinct("after", index)).await;
    }
    assert_eq!(dropped_diagnostics(&client, &fixture).await - shed, 3);
    assert_eq!(stored(&client, &fixture).await, 60);

    // The station is still over budget. A redelivery of the stored first event (same type and
    // timestamp as `flood-0`, new call id) is recognised before it can spend or be shed.
    let before = dropped_diagnostics(&client, &fixture).await;
    notify(&mut socket, distinct("again", 0)).await;
    assert_eq!(dropped_diagnostics(&client, &fixture).await, before);
    assert_eq!(stored(&client, &fixture).await, 60);
}
