//! Large reports stay inside the shared multipart limits: 1 MiB, 4096 items, 256 fragments.
use crate::harness::{Harness, corpus};
use crate::reports::{complete, failure, fragment, monitor, push, request};
use serde_json::{Value, json};
use uob_contracts::*;

async fn start(h: &mut Harness, request_id: i64) {
    let id = h.id();
    request(
        h,
        &id,
        &json!({"requestId": request_id}),
        corpus("get-report-reply-accepted"),
    )
    .await;
}

/// One variable carrying `count` tiny monitors.
fn crowded(request_id: i64, sequence: u32, more: bool, first: i64, count: i64) -> Value {
    let monitors: Vec<Value> = (first..first + count)
        .map(|id| json!({"id": id, "transaction": false, "value": 1, "type": "Periodic", "severity": 5}))
        .collect();
    fragment(
        request_id,
        sequence,
        more,
        vec![json!({
            "component": {"name": "A"}, "variable": {"name": "B"}, "variableMonitoring": monitors,
        })],
    )
}

/// Monitors with maximal names: about 330 encoded bytes each, so bytes run out before items.
fn heavy(request_id: i64, sequence: u32, more: bool, first: usize, count: usize) -> Value {
    let name = |letter: char| letter.to_string().repeat(50);
    let monitors: Vec<Value> = (first..first + count)
        .map(|index| {
            json!({
                "component": {"name": name('c'), "instance": name('i')},
                "variable": {"name": name('v'), "instance": name('j')},
                "variableMonitoring": [{
                    "id": 1_000_000 + index, "transaction": false, "value": 1,
                    "type": "Periodic", "severity": 5,
                }],
            })
        })
        .collect();

    fragment(request_id, sequence, more, monitors)
}

#[tokio::test]
async fn more_than_4096_monitors_fail_the_item_limit_with_progress() {
    let mut h = Harness::new().await;
    start(&mut h, 8001).await;
    // Each fragment's typed expansion stays under the per-message byte limit.
    push(&mut h, crowded(8001, 0, true, 0, 1400)).await;
    push(&mut h, crowded(8001, 1, true, 1400, 1400)).await;
    push(&mut h, crowded(8001, 2, false, 2800, 1400)).await;

    let (_, report) = h.reports.merged().await;
    let (reason, progress) = failure(&report);
    assert_eq!(reason, DeviceReportFailure201::ItemLimit);
    let progress = progress.unwrap();
    assert_eq!((progress.fragments, progress.items), (2, 2800));
    h.close().await;
}

#[tokio::test]
async fn exactly_4096_monitors_are_complete() {
    let mut h = Harness::new().await;
    start(&mut h, 8002).await;
    push(&mut h, crowded(8002, 0, true, 0, 1400)).await;
    push(&mut h, crowded(8002, 1, true, 1400, 1400)).await;
    push(&mut h, crowded(8002, 2, false, 2800, 1296)).await;

    let (_, report) = h.reports.merged().await;
    let (fragments, monitors, omitted) = complete(&report);
    assert_eq!((fragments.len(), monitors.len(), omitted), (3, 4096, 0));
    assert_eq!(monitors[4095].id, 4095);
    h.close().await;
}

#[tokio::test]
async fn more_than_one_mebibyte_of_monitors_fails_the_byte_limit() {
    let mut h = Harness::new().await;
    start(&mut h, 8003).await;
    // About 330 encoded bytes each: five fragments of 640 pass 1 MiB before 4096 items.
    for sequence in 0..5 {
        push(
            &mut h,
            heavy(8003, sequence, true, sequence as usize * 640, 640),
        )
        .await;
    }

    let (_, report) = h.reports.merged().await;
    let (reason, progress) = failure(&report);
    assert_eq!(reason, DeviceReportFailure201::ByteLimit);
    let progress = progress.unwrap();
    assert!(progress.bytes <= 1024 * 1024 && progress.items < 4096);
    h.close().await;
}

#[tokio::test]
async fn a_report_of_257_fragments_fails_the_fragment_limit() {
    let mut h = Harness::new().await;
    start(&mut h, 8004).await;
    for sequence in 0..256 {
        push(
            &mut h,
            fragment(
                8004,
                sequence,
                true,
                vec![monitor("A", "B", i64::from(sequence))],
            ),
        )
        .await;
    }

    let (_, report) = h.reports.merged().await;
    let (reason, progress) = failure(&report);
    assert_eq!(reason, DeviceReportFailure201::FragmentLimit);
    assert_eq!(progress.map(|p| p.fragments), Some(255));
    h.close().await;
}

#[tokio::test]
async fn the_fragment_limit_itself_is_still_complete() {
    let mut h = Harness::new().await;
    start(&mut h, 8005).await;
    for sequence in 0..255 {
        push(
            &mut h,
            fragment(
                8005,
                sequence,
                true,
                vec![monitor("A", "B", i64::from(sequence))],
            ),
        )
        .await;
    }
    push(
        &mut h,
        fragment(8005, 255, false, vec![monitor("A", "B", 255)]),
    )
    .await;

    let (_, report) = h.reports.merged().await;
    let (fragments, monitors, omitted) = complete(&report);
    assert_eq!((fragments.len(), monitors.len(), omitted), (256, 256, 0));
    h.close().await;
}

#[tokio::test]
async fn a_report_filling_the_byte_limit_keeps_the_result_within_its_output_limit() {
    let mut h = Harness::new().await;
    start(&mut h, 8006).await;
    // Fill the shared 1 MiB collection limit to within a few hundred bytes.
    let sample = ReportedMonitor201 {
        component: DeviceComponent201 {
            name: "c".repeat(50),
            instance: Some("i".repeat(50)),
            evse: None,
        },
        variable: DeviceVariable201 {
            name: "v".repeat(50),
            instance: Some("j".repeat(50)),
        },
        id: 1_000_000,
        transaction: false,
        value: "1".parse().unwrap(),
        monitor_type: MonitorType201::Periodic,
        severity: MonitorSeverity201::new(5).unwrap(),
    };
    let size = serde_json::to_vec(&sample).unwrap().len();
    let total = (1024 * 1024 - 200) / size;
    let mut sent = 0;
    let mut sequence = 0;
    while sent < total {
        let count = (total - sent).min(640);
        let last = sent + count == total;
        push(&mut h, heavy(8006, sequence, !last, sent, count)).await;
        sent += count;
        sequence += 1;
    }

    let (_, report) = h.reports.merged().await;
    let (fragments, monitors, omitted) = complete(&report);
    // Everything received is accounted for: kept, or counted as omitted. Nothing vanishes.
    assert!(omitted > 0, "the result limit must have bound");
    assert_eq!(monitors.len() + omitted as usize, total);
    assert_eq!(
        fragments.iter().map(|f| f.monitors as usize).sum::<usize>(),
        total
    );
    let wrapped = MonitoringResult201 {
        query: MonitoringQuery201::GetMonitoringReport {
            request_id: 8006,
            criteria: vec![],
            component_variables: vec![],
        },
        outcome: MonitoringOutcome201::Report {
            ack: Some(DeviceReportAck201::Accepted),
            report: report.clone(),
        },
    };
    assert!(serde_json::to_vec(&wrapped).unwrap().len() <= MONITORING_REPORT_OUTPUT_LIMIT_201);
    h.close().await;
}
