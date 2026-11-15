use super::support::*;
use serde_json::{Value, json};

fn report(result: &Value) -> &Value {
    &result["monitoring_201"]["outcome"]["report"]
}

#[tokio::test]
async fn sequence_gaps_and_duplicates_end_the_report_incomplete_with_their_reason() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    start_report(&client, &fixture, &mut socket, "gap", 7101).await;
    deliver(&mut socket, fragment("g0", 7101, 0, true, &[4001])).await;
    exchange(&mut socket, fragment("g2", 7101, 2, false, &[4002])).await;
    let gap = settled(&client, &fixture, "gap").await;
    assert_eq!(report(&gap)["state"], "incomplete");
    assert_eq!(report(&gap)["reason"], "out_of_order_sequence");

    start_report(&client, &fixture, &mut socket, "duplicate", 7102).await;
    deliver(&mut socket, fragment("d0", 7102, 0, true, &[4001])).await;
    exchange(&mut socket, fragment("d0-again", 7102, 0, true, &[4001])).await;
    let duplicate = settled(&client, &fixture, "duplicate").await;
    assert_eq!(report(&duplicate)["state"], "incomplete");
    assert_eq!(report(&duplicate)["reason"], "duplicate_sequence");

    // Fragments of a different request never join a report; the correct one still completes.
    start_report(&client, &fixture, &mut socket, "mixed", 7103).await;
    exchange(&mut socket, fragment("s0", 7999, 0, true, &[4002, 4003])).await;
    deliver(&mut socket, fragment("m0", 7103, 0, false, &[4001])).await;
    let complete = settled(&client, &fixture, "mixed").await;
    assert_eq!(report(&complete)["state"], "complete");
    assert_eq!(report(&complete)["monitors"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_report_beyond_the_shared_fragment_limit_is_incomplete_with_the_limit_reason() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    start_report(&client, &fixture, &mut socket, "fragments", 7201).await;
    for sequence in 0..260_u32 {
        let reply = exchange(
            &mut socket,
            fragment(&format!("f{sequence}"), 7201, sequence, true, &[1]),
        )
        .await;
        if reply[0] == 4 {
            break;
        }
        // Fragments wait in a four-deep queue, and a loopback station can outrun the collector
        // into `capacity`. A real link is slower than this pause.
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let limited = settled(&client, &fixture, "fragments").await;
    assert_eq!(report(&limited)["state"], "incomplete");
    assert_eq!(report(&limited)["reason"], "fragment_limit");
    // The station stays connected and answered.
    let heartbeat = station_call(&mut socket, "beat", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
}

#[tokio::test]
async fn a_report_beyond_the_shared_item_limit_is_incomplete_with_the_limit_reason() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    start_report(&client, &fixture, &mut socket, "items", 7301).await;
    // Three large fragments carry more monitors than the shared item limit allows.
    for sequence in 0..3_u32 {
        let monitors: Vec<i32> = (0..1500).collect();
        let reply = exchange(
            &mut socket,
            fragment(&format!("i{sequence}"), 7301, sequence, true, &monitors),
        )
        .await;
        if reply[0] == 4 {
            break;
        }
    }
    let limited = settled(&client, &fixture, "items").await;
    assert_eq!(report(&limited)["state"], "incomplete");
    assert_eq!(report(&limited)["reason"], "item_limit");
}

#[tokio::test]
async fn a_disconnect_mid_report_ends_it_incomplete_as_disconnected() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    start_report(&client, &fixture, &mut socket, "cancelled", 7401).await;
    deliver(&mut socket, fragment("c0", 7401, 0, true, &[4001])).await;
    drop(socket);
    let result = settled(&client, &fixture, "cancelled").await;
    assert_eq!(report(&result)["state"], "incomplete");
    assert_eq!(report(&result)["reason"], "disconnected");
    assert_eq!(result["monitoring_201"]["outcome"]["ack"], "Accepted");
}

#[tokio::test]
async fn a_native_refusal_expects_no_report_and_a_restart_interrupts_a_pending_one() {
    let (fixture, process) = launch(ALL_OPTIONS).await;
    let client = client();
    {
        let mut socket = connected(&fixture, &client).await;
        let (_, refused) = answered(
            &client,
            &fixture,
            &mut socket,
            command("empty", "GetMonitoringReport", report_request(7501)),
            json!({"status":"EmptyResultSet"}),
        )
        .await;
        assert_eq!(report(&refused)["state"], "not_expected");

        start_report(&client, &fixture, &mut socket, "pending", 7502).await;
        deliver(&mut socket, fragment("p0", 7502, 0, true, &[4001])).await;
        // The daemon dies mid-report.
        drop(process);
    }

    let _restarted = restart(&fixture).await;
    let recovered = result(&client, &fixture, "pending").await;
    assert_eq!(recovered["lifecycle"]["stage"], "protocol_response");
    assert_eq!(recovered["monitoring_201"]["outcome"]["ack"], "Accepted");
    assert_eq!(report(&recovered)["state"], "incomplete");
    assert_eq!(report(&recovered)["reason"], "interrupted");
    // The restarted daemon never repeats the request on a later connection.
    let mut socket = connected(&fixture, &client).await;
    no_call(&mut socket).await;
}
