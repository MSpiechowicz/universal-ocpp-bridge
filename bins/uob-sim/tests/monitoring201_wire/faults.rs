//! The deliberate misbehaviour knobs, as the CSMS sees them on the socket.
use crate::monitoring201_support::{connector, evse, item, station_component};
use crate::socket::{accept, boot, closed, receive, send};
use crate::{call, expect_call, quiet, run, station};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use uob_sim::ProtocolClient;

/// Four custom monitors: with the two seeded ones, three fragments of two.
async fn populate(socket: &mut crate::Socket) {
    let request = json!({"setMonitoringData": [
        item(&connector(), "Temperature", "UpperThreshold", 75.0, 3),
        item(&connector(), "Temperature", "LowerThreshold", -10.0, 4),
        item(&evse(), "Power", "Delta", 2.5, 6),
        item(&station_component(), "SupplyVoltage", "LowerThreshold", 207.0, 4)]});
    let reply = call(socket, "set", "SetVariableMonitoring", request).await;
    assert_eq!(reply[2]["setMonitoringResult"].as_array().unwrap().len(), 4);
}

fn faulty(fault: &str) -> Value {
    json!({"monitors_per_fragment": 2, "report_fault": fault})
}

async fn fragments(socket: &mut crate::Socket, count: usize) -> Vec<i64> {
    let mut sequence = Vec::new();
    for _ in 0..count {
        let fragment = expect_call(socket, "NotifyMonitoringReport").await;
        assert_eq!(fragment["requestId"], 5);
        sequence.push(fragment["seqNo"].as_i64().unwrap());
    }
    sequence
}

#[tokio::test]
async fn a_skipped_and_a_duplicated_sequence_number_reach_the_csms() {
    for (fault, expected) in [("skip_seq_no", [0, 2, 3]), ("duplicate_seq_no", [0, 0, 1])] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = async {
            let mut socket = accept(&listener).await;
            boot(&mut socket).await;
            populate(&mut socket).await;
            let reply = call(
                &mut socket,
                "report",
                "GetMonitoringReport",
                json!({"requestId": 5}),
            )
            .await;
            assert_eq!(reply[2]["status"], "Accepted");
            assert_eq!(fragments(&mut socket, 3).await, expected, "{fault}");
        };
        let client = run(peer, station(&listener, faulty(fault))).await;
        client.abort();
    }
}

#[tokio::test]
async fn the_final_fragment_is_never_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        populate(&mut socket).await;
        call(
            &mut socket,
            "report",
            "GetMonitoringReport",
            json!({"requestId": 5}),
        )
        .await;
        let mut tbc = Vec::new();
        for _ in 0..2 {
            let fragment = expect_call(&mut socket, "NotifyMonitoringReport").await;
            tbc.push(fragment["tbc"].as_bool().unwrap());
        }
        assert_eq!(tbc, [true, true]);
        assert!(quiet(&mut socket).await, "the chain stays open");
    };
    let client = run(peer, station(&listener, faulty("omit_final_fragment"))).await;
    let snapshot = client.monitoring201().unwrap().snapshot();
    assert_eq!(
        snapshot["lastReport"],
        json!({"requestId": 5, "planned": 3, "sent": 2, "outcome": "Withheld"})
    );
    assert_eq!(snapshot["reportsDelivered"], 0);
    client.abort();
}

#[tokio::test]
async fn the_socket_closes_in_the_middle_of_a_report() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        populate(&mut socket).await;
        call(
            &mut socket,
            "report",
            "GetMonitoringReport",
            json!({"requestId": 5}),
        )
        .await;
        assert_eq!(fragments(&mut socket, 1).await, [0]);
        closed(&mut socket).await;
    };
    let client = run(peer, station(&listener, faulty("disconnect_mid_report"))).await;
    let snapshot = client.monitoring201().unwrap().snapshot();
    assert_eq!(
        snapshot["lastReport"],
        json!({"requestId": 5, "planned": 3, "sent": 1, "outcome": "Interrupted"})
    );
    client.abort();
}

#[tokio::test]
async fn fragments_may_arrive_before_the_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = async {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        populate(&mut socket).await;
        send(
            &mut socket,
            json!([2, "report", "GetMonitoringReport", {"requestId": 5}]),
        )
        .await;
        assert_eq!(fragments(&mut socket, 3).await, [0, 1, 2]);
        let reply = receive(&mut socket).await;
        assert_eq!(
            (reply[0].as_u64(), reply[1].as_str(), &reply[2]),
            (Some(3), Some("report"), &json!({"status": "Accepted"}))
        );
    };
    let client = run(peer, station(&listener, faulty("fragments_before_reply"))).await;
    client.abort();
}

#[tokio::test]
async fn a_set_reply_with_the_wrong_count_or_component_reaches_the_csms() {
    for (fault, count) in [("wrong_result_count", 5), ("mismatched_component", 4)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = async {
            let mut socket = accept(&listener).await;
            boot(&mut socket).await;
            let request = json!({"setMonitoringData": [
                item(&connector(), "Temperature", "UpperThreshold", 75.0, 3),
                item(&connector(), "Temperature", "LowerThreshold", -10.0, 4),
                item(&evse(), "Power", "Delta", 2.5, 6),
                item(&station_component(), "SupplyVoltage", "LowerThreshold", 207.0, 4)]});
            let reply = call(&mut socket, "set", "SetVariableMonitoring", request.clone()).await;
            let results = reply[2]["setMonitoringResult"].as_array().unwrap();
            assert_eq!(results.len(), count, "{fault}");
            if fault == "mismatched_component" {
                assert_ne!(
                    results[0]["component"],
                    request["setMonitoringData"][0]["component"]
                );
            }
        };
        let client = run(peer, station(&listener, json!({"set_reply_fault": fault}))).await;
        client.abort();
    }
}
