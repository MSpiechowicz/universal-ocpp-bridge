use super::support::*;
use serde_json::{Value, json};

async fn notify(socket: &mut Socket, frame: Value) {
    let id = frame[1].clone();
    send(socket, frame).await;
    assert_eq!(receive(socket).await, json!([3, id, {}]));
}

fn record(event: &Value) -> &Value {
    &event["payload"]["monitoring_events_201"]
}

#[tokio::test]
async fn an_event_burst_is_acknowledged_and_journaled_one_bounded_record_per_call() {
    let (fixture, _process) = launch("").await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let mut journal = Journal::open(&client, &fixture).await;

    // A chain of three messages, then a message after a skipped number, then a restarted series.
    notify(&mut socket, event_call("e0", 0, true, 2)).await;
    notify(&mut socket, event_call("e1", 1, true, 1)).await;
    notify(&mut socket, event_call("e2", 2, false, 1)).await;
    notify(&mut socket, event_call("gap", 3, false, 1)).await;
    notify(&mut socket, event_call("s0", 0, true, 1)).await;
    notify(&mut socket, event_call("again", 0, true, 1)).await;
    // More events than one record keeps: the first 32 are retained and the rest are counted.
    notify(&mut socket, event_call("big", 1, false, 40)).await;

    let mut events = Vec::new();
    for _ in 0..7 {
        events.push(journal.next().await);
    }
    assert!(journal.quiet().await);

    let shapes: Vec<_> = events
        .iter()
        .map(|event| {
            let record = record(event);
            (
                record["seq_no"].as_u64().unwrap(),
                record["tbc"].as_bool().unwrap(),
                record["events"].as_array().unwrap().len(),
                record["ordering"].clone(),
            )
        })
        .collect();
    assert_eq!(
        shapes,
        [
            (0, true, 2, json!({"kind":"in_sequence"})),
            (1, true, 1, json!({"kind":"in_sequence"})),
            (2, false, 1, json!({"kind":"in_sequence"})),
            (3, false, 1, json!({"kind":"gap","expected":0})),
            (0, true, 1, json!({"kind":"in_sequence"})),
            (0, true, 1, json!({"kind":"restarted"})),
            (1, false, 32, json!({"kind":"in_sequence"})),
        ]
    );
    assert_eq!(record(&events[6])["events_omitted"], 8);
    assert!(record(&events[0]).get("shed_calls").is_none());
    let sequences: Vec<_> = events
        .iter()
        .map(|event| event["sequence"].as_u64().unwrap())
        .collect();
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    for event in &events {
        assert_eq!(event["event_type"], "station.monitoring_event.201");
        assert_eq!(event["origin"]["kind"], "station");
        assert_eq!(event["resource"]["station_id"], "station-a");
    }
    // Native event facts keep their wire values and order.
    let first = &record(&events[0])["events"];
    assert_eq!(first[0]["event_id"], 9000);
    assert_eq!(first[1]["event_id"], 9001);
    assert_eq!(first[0]["trigger"], "Alerting");
    assert_eq!(first[0]["notification"], "CustomMonitor");
    assert_eq!(first[0]["variable_monitoring_id"], 4001);
    assert_eq!(first[0]["actual_value"], "81.0");

    // A subscriber that connects later replays the identical retained records.
    let mut late = Journal::open(&client, &fixture).await;
    for original in &events {
        assert_eq!(&late.next().await, original);
    }
}

#[tokio::test]
async fn a_flood_is_acknowledged_shed_and_signalled_without_dropping_the_session() {
    let (fixture, _process) = launch("").await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let before = dropped_diagnostics(&client, &fixture).await;

    // The per-station budget is 60 records per minute. Every CALL still gets the empty reply.
    for index in 0..100_u32 {
        notify(
            &mut socket,
            event_call(&format!("flood-{index}"), 0, false, 1),
        )
        .await;
    }

    let mut journal = Journal::open(&client, &fixture).await;
    let mut stored = 0;
    for _ in 0..60 {
        let event = journal.next().await;
        assert!(record(&event).get("shed_calls").is_none());
        stored += 1;
    }
    assert!(
        journal.quiet().await,
        "nothing beyond the budget is journaled"
    );
    assert_eq!(stored, 60);
    // The shed CALLs raised the shared resource-pressure signal.
    assert_eq!(dropped_diagnostics(&client, &fixture).await - before, 40);
    // The session survives the flood and keeps serving.
    let heartbeat = station_call(&mut socket, "beat", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
    assert_eq!(
        fixture.snapshot(&client, "station-a").await["connectivity"]["status"],
        "connected"
    );

    // The budget belongs to the station: reconnecting neither refreshes it nor forgets what was
    // shed. The new connection's calls are still acknowledged, but nothing is journaled until
    // the window rolls over, and each one is counted as shed.
    let shed = dropped_diagnostics(&client, &fixture).await;
    drop(socket);
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    for index in 0..3_u32 {
        notify(
            &mut socket,
            event_call(&format!("after-reconnect-{index}"), 0, false, 1),
        )
        .await;
    }
    assert_eq!(dropped_diagnostics(&client, &fixture).await - shed, 3);
    let mut journal = Journal::open(&client, &fixture).await;
    for _ in 0..60 {
        let event = journal.next().await;
        assert!(record(&event).get("shed_calls").is_none());
    }
    assert!(
        journal.quiet().await,
        "a reconnect does not refresh the budget"
    );
    let heartbeat = station_call(&mut socket, "beat-2", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
}

#[tokio::test]
async fn events_are_handled_without_any_option_and_the_commands_stay_unavailable() {
    let (fixture, _process) = launch("").await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let mut journal = Journal::open(&client, &fixture).await;

    notify(&mut socket, event_call("always-on", 0, false, 1)).await;
    let event = journal.next().await;
    assert_eq!(record(&event)["events"][0]["event_id"], 9000);

    let advertised =
        fixture.snapshot(&client, "station-a").await["capabilities"]["operations"].to_string();
    for (action, payload) in [
        ("SetVariableMonitoring", json!({"setMonitoringData":[]})),
        ("ClearVariableMonitoring", json!({"id":[1]})),
        ("GetMonitoringReport", report_request(1)),
        ("SetMonitoringBase", json!({"monitoringBase":"All"})),
        ("SetMonitoringLevel", json!({"severity":3})),
    ] {
        assert!(!advertised.contains(action), "{action}");
        let (status, _) = refused(&client, &fixture, command(action, action, payload)).await;
        assert!((400..500).contains(&status), "{action}: {status}");
    }
    // Report fragments nobody asked for are not accepted either.
    let unsolicited = exchange(&mut socket, fragment("stray", 1, 0, false, &[1])).await;
    assert_eq!(unsolicited[0], 4);
    no_call(&mut socket).await;
}

#[tokio::test]
async fn invalid_unregistered_and_ocpp16_events_are_not_journaled_but_sloppy_ones_are_sanitized() {
    let (fixture, _process) = launch("").await;
    let client = client();

    // Before the station is accepted its events are refused, whatever their content.
    let mut early = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    let reply = exchange(&mut early, event_call("early", 0, false, 1)).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "ProtocolError");
    boot(&mut early).await;
    fixture.connected(&client, "station-a").await;
    let mut journal = Journal::open(&client, &fixture).await;
    assert!(journal.quiet().await);

    // A schema-invalid event gets a native error that never echoes content.
    let mut invalid = event_call("empty", 0, false, 1);
    invalid[3]["eventData"] = json!([]);
    let reply = exchange(&mut early, invalid).await;
    assert_eq!(reply[0], 4);
    assert!(!reply.to_string().contains("SECRET-TEXT"));
    assert!(journal.quiet().await);

    // A schema-valid event is always acknowledged, however the charger wrote it: control
    // characters are replaced and a missing monitor id stays absent, so a queued event can never
    // wedge the charger.
    let mut control = event_call("control", 0, false, 1);
    control[3]["eventData"][0]["actualValue"] = json!("SECRET-TEXT\u{7}\t");
    control[3]["eventData"][0]["techInfo"] = json!("line\nbreak");
    notify(&mut early, control).await;
    let sanitized = journal.next().await;
    assert_eq!(
        record(&sanitized)["events"][0]["actual_value"],
        "SECRET-TEXT\u{fffd}\u{fffd}"
    );
    assert_eq!(
        record(&sanitized)["events"][0]["tech_info"],
        "line\u{fffd}break"
    );
    // The earlier refusal did not disturb the ordering of the next valid call.
    assert_eq!(
        record(&sanitized)["ordering"],
        json!({"kind":"in_sequence"})
    );

    let mut anonymous = event_call("anonymous", 0, false, 1);
    anonymous[3]["eventData"][0]
        .as_object_mut()
        .unwrap()
        .remove("variableMonitoringId");
    notify(&mut early, anonymous).await;
    let anonymous = journal.next().await;
    assert!(record(&anonymous)["events"][0]["variable_monitoring_id"].is_null());
    assert_eq!(
        record(&anonymous)["events"][0]["notification"],
        "CustomMonitor"
    );

    // OCPP 1.6 has no NotifyEvent.
    let mut legacy = fixture.station("station-b").await;
    crate::host::boot(&mut legacy).await;
    let reply = exchange(&mut legacy, event_call("legacy", 0, false, 1)).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "NotImplemented");
    assert!(journal.quiet().await);
}
