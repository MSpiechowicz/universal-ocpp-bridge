use crate::harness::{Harness, corpus, evidence, negative};
use serde_json::{Value, json};
use uob_application::CommandDispatchOutcome;
use uob_contracts::*;

pub const ACTION: &str = "GetMonitoringReport";
const NOTIFY: &str = "NotifyMonitoringReport";

/// Dispatches `GetMonitoringReport` and answers it with `ack`, returning the command outcome.
pub async fn request(
    h: &mut Harness,
    id: &str,
    payload: &Value,
    ack: Value,
) -> MonitoringResult201 {
    let dispatched = h.dispatch(id, ACTION, payload.clone());
    h.call(id, ACTION, payload).await;
    h.reply(id, ack).await;
    evidence(dispatched.await.unwrap())
}

pub fn report_of(result: &MonitoringResult201) -> &MonitoringReportState201 {
    let MonitoringOutcome201::Report { report, .. } = &result.outcome else {
        panic!("report outcome");
    };
    report
}

/// A fragment acknowledged with the empty native response.
pub async fn push(h: &mut Harness, payload: Value) {
    let id = h.id();
    let answer = h.notify(&id, NOTIFY, payload).await;
    assert_eq!(answer, json!([3, id, {}]));
}

pub fn fragment(request_id: i64, sequence: u32, more: bool, monitors: Vec<Value>) -> Value {
    json!({
        "requestId": request_id, "seqNo": sequence, "tbc": more,
        "generatedAt": "2026-11-10T09:15:00Z", "monitor": Value::Array(monitors),
    })
}

pub fn monitor(component: &str, variable: &str, id: i64) -> Value {
    json!({
        "component": {"name": component}, "variable": {"name": variable},
        "variableMonitoring": [{"id": id, "transaction": false, "value": 10, "type": "Periodic", "severity": 5}],
    })
}

/// The merged terminal report, whether it travelled in the outcome or through the store.
pub async fn terminal(h: &Harness, result: &MonitoringResult201) -> MonitoringReportState201 {
    match report_of(result) {
        MonitoringReportState201::Pending => h.reports.merged().await.1,
        done => done.clone(),
    }
}

pub fn complete(
    report: &MonitoringReportState201,
) -> (&[MonitoringReportFragment201], &[ReportedMonitor201], u32) {
    let MonitoringReportState201::Complete {
        fragments,
        monitors,
        monitors_omitted,
    } = report
    else {
        panic!("expected a complete report, got {report:?}");
    };
    (fragments, monitors, *monitors_omitted)
}

pub fn failure(
    report: &MonitoringReportState201,
) -> (DeviceReportFailure201, Option<DeviceReportProgress201>) {
    let MonitoringReportState201::Incomplete { reason, progress } = report else {
        panic!("expected an incomplete report, got {report:?}");
    };
    (*reason, *progress)
}

#[tokio::test]
async fn a_single_fragment_report_completes_with_the_exact_monitors() {
    let mut h = Harness::new().await;
    let id = h.id();
    let result = request(
        &mut h,
        &id,
        &corpus("get-report-no-filter"),
        corpus("get-report-reply-accepted"),
    )
    .await;
    assert_eq!(
        result.outcome,
        MonitoringOutcome201::Report {
            ack: Some(DeviceReportAck201::Accepted),
            report: MonitoringReportState201::Pending
        }
    );
    assert!(result.accepted() && result.pending());
    assert_eq!(
        result.query,
        MonitoringQuery201::GetMonitoringReport {
            request_id: 7001,
            criteria: vec![],
            component_variables: vec![]
        }
    );

    push(&mut h, corpus("report-single")).await;
    let (request, report) = h.reports.merged().await;
    assert_eq!(request.as_str(), id);
    let (fragments, monitors, omitted) = complete(&report);
    assert_eq!(omitted, 0);
    assert_eq!(fragments.len(), 1);
    assert_eq!(
        (
            fragments[0].sequence,
            fragments[0].more,
            fragments[0].monitors
        ),
        (0, false, 3)
    );
    let seen: Vec<_> = monitors
        .iter()
        .map(|m| {
            (
                m.component.name.as_str(),
                m.variable.name.as_str(),
                m.id,
                m.transaction,
                m.value.to_string(),
                m.monitor_type,
                m.severity.get(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                "Connector",
                "Temperature",
                4001,
                false,
                "75".to_owned(),
                MonitorType201::UpperThreshold,
                3
            ),
            (
                "EVSE",
                "Power",
                4003,
                true,
                "2.5".to_owned(),
                MonitorType201::Delta,
                6
            ),
            (
                "EVSE",
                "Power",
                4007,
                false,
                "22000".to_owned(),
                MonitorType201::UpperThreshold,
                2
            ),
        ]
    );
    assert_eq!(
        monitors[0].component.evse,
        Some(DeviceEvse201 {
            id: 1,
            connector_id: Some(1)
        })
    );
    h.close().await;
}

#[tokio::test]
async fn three_fragments_assemble_in_native_order_under_a_component_filter() {
    let mut h = Harness::new().await;
    h.learn_from("DeviceDataCtrlr", "GetReport", "ItemsPerMessage", "8")
        .await;
    h.learn_from("DeviceDataCtrlr", "GetReport", "BytesPerMessage", "100000")
        .await;
    let id = h.id();
    let result = request(
        &mut h,
        &id,
        &corpus("get-report-component-variable"),
        corpus("get-report-reply-accepted"),
    )
    .await;
    assert!(result.pending());
    for name in [
        "report-fragment-0",
        "report-fragment-1",
        "report-fragment-2",
    ] {
        push(&mut h, corpus(name)).await;
    }

    let (_, report) = h.reports.merged().await;
    let (fragments, monitors, omitted) = complete(&report);
    assert_eq!(omitted, 0);
    let order: Vec<_> = fragments
        .iter()
        .map(|f| (f.sequence, f.more, f.monitors))
        .collect();
    assert_eq!(order, [(0, true, 1), (1, true, 1), (2, false, 2)]);
    let ids: Vec<_> = monitors.iter().map(|m| m.id).collect();
    assert_eq!(ids.len(), 4);
    assert_eq!(monitors[0].variable.name, "Power");
    assert_eq!(monitors[3].component.instance.as_deref(), Some("Cabinet"));
    assert_eq!(monitors[3].variable.instance.as_deref(), Some("Inlet"));
    h.close().await;
}

#[tokio::test]
async fn fragments_that_arrive_before_the_reply_complete_the_same_report() {
    let mut h = Harness::new().await;
    let id = h.id();
    let payload = corpus("get-report-no-filter");
    let dispatched = h.dispatch(&id, ACTION, payload.clone());
    h.call(&id, ACTION, &payload).await;
    push(&mut h, corpus("report-single")).await;
    h.reply(&id, corpus("get-report-reply-accepted")).await;

    let result = evidence(dispatched.await.unwrap());
    let report = terminal(&h, &result).await;
    let (fragments, monitors, _) = complete(&report);
    assert_eq!((fragments.len(), monitors.len()), (1, 3));
    h.close().await;
}

#[tokio::test]
async fn other_request_ids_and_unsolicited_fragments_are_acknowledged_and_ignored() {
    let mut h = Harness::new().await;
    // Well formed but nobody asked: acknowledged, never collected.
    push(&mut h, corpus("report-single")).await;
    assert_eq!(h.reports.attempts(), 0);

    let id = h.id();
    request(
        &mut h,
        &id,
        &corpus("get-report-no-filter"),
        corpus("get-report-reply-accepted"),
    )
    .await;
    let mut other = corpus("report-single");
    other["requestId"] = json!(9999);
    push(&mut h, other).await;
    assert_eq!(h.reports.attempts(), 0);

    push(
        &mut h,
        fragment(7001, 0, false, vec![monitor("Heater", "Temperature", 4100)]),
    )
    .await;
    let (_, report) = h.reports.merged().await;
    let (_, monitors, _) = complete(&report);
    assert_eq!(monitors.len(), 1);
    assert_eq!(monitors[0].id, 4100);

    // The route retired with the final fragment: a late one is acknowledged and ignored.
    push(
        &mut h,
        fragment(7001, 1, false, vec![monitor("Heater", "Temperature", 4101)]),
    )
    .await;
    assert_eq!(h.reports.recorded().len(), 1);
    h.close().await;
}

#[tokio::test]
async fn a_report_that_was_not_accepted_expects_no_fragments() {
    for (ack, status) in [
        (
            "get-report-reply-empty-result-set",
            DeviceReportAck201::EmptyResultSet,
        ),
        (
            "get-report-reply-not-supported",
            DeviceReportAck201::NotSupported,
        ),
        ("get-report-reply-rejected", DeviceReportAck201::Rejected),
    ] {
        let mut h = Harness::new().await;
        let id = h.id();
        let result = request(&mut h, &id, &corpus("get-report-no-filter"), corpus(ack)).await;
        assert_eq!(
            result.outcome,
            MonitoringOutcome201::Report {
                ack: Some(status),
                report: MonitoringReportState201::NotExpected
            },
            "{ack}"
        );
        assert!(!result.accepted() && !result.pending());
        // Nothing is collected or merged for it.
        push(&mut h, corpus("report-single")).await;
        assert_eq!(h.reports.attempts(), 0, "{ack}");
        h.close().await;
    }
}

#[tokio::test]
async fn fragments_behind_a_rejection_are_a_correlation_failure_not_a_silent_drop() {
    let mut h = Harness::new().await;
    let id = h.id();
    let payload = corpus("get-report-no-filter");
    let dispatched = h.dispatch(&id, ACTION, payload.clone());
    h.call(&id, ACTION, &payload).await;
    push(&mut h, corpus("report-single")).await;
    h.reply(&id, corpus("get-report-reply-rejected")).await;

    let result = evidence(dispatched.await.unwrap());
    assert_eq!(
        result.outcome,
        MonitoringOutcome201::Report {
            ack: Some(DeviceReportAck201::Rejected),
            report: MonitoringReportState201::Incomplete {
                reason: DeviceReportFailure201::Correlation,
                progress: None
            },
        }
    );
    assert!(!result.accepted());
    assert_eq!(h.reports.attempts(), 0);
    h.close().await;
}

#[tokio::test]
async fn an_invalid_acknowledgement_is_never_evidence() {
    let mut h = Harness::new().await;
    let replies = [
        json!({"status": "Maybe"}),
        json!({}),
        json!({"status": "Accepted", "extra": 1}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "x\u{7}"}}),
    ];
    for (offset, reply) in replies.into_iter().enumerate() {
        let id = h.id();
        let payload = json!({"requestId": 7100 + offset});
        let dispatched = h.dispatch(&id, ACTION, payload.clone());
        h.call(&id, ACTION, &payload).await;
        h.reply(&id, reply.clone()).await;
        let outcome = dispatched.await.unwrap();
        assert!(
            matches!(
                outcome,
                CommandDispatchOutcome::TransmissionUncertain { .. }
            ),
            "{reply} -> {outcome:?}"
        );
    }

    // A native request ID is never reused on one connection, even after a failed exchange.
    let id = h.id();
    let again = h
        .dispatch(&id, ACTION, json!({"requestId": 7100}))
        .await
        .unwrap();
    assert!(
        matches!(again, CommandDispatchOutcome::NotTransmitted { .. }),
        "{again:?}"
    );
    h.no_extra_call().await;
    h.close().await;
}

#[tokio::test]
async fn monitors_outside_the_request_are_a_correlation_failure() {
    for (request_case, report_case, extra_learning) in [
        ("get-report-criteria", "report-outside-criteria", false),
        (
            "get-report-component-variable",
            "report-outside-component-filter",
            true,
        ),
        ("get-report-both", "report-variable-outside-filter", false),
    ] {
        let mut h = Harness::new().await;
        if extra_learning {
            h.learn_from("DeviceDataCtrlr", "GetReport", "ItemsPerMessage", "8")
                .await;
            h.learn_from("DeviceDataCtrlr", "GetReport", "BytesPerMessage", "100000")
                .await;
        }
        let id = h.id();
        request(
            &mut h,
            &id,
            &corpus(request_case),
            corpus("get-report-reply-accepted"),
        )
        .await;
        // The charger's message is well formed: it is acknowledged, then the report fails.
        push(&mut h, negative(report_case)).await;
        let (_, report) = h.reports.merged().await;
        let (reason, progress) = failure(&report);
        assert_eq!(reason, DeviceReportFailure201::Correlation, "{report_case}");
        assert_eq!(progress.map(|p| p.items), Some(0), "{report_case}");
        h.close().await;
    }
}
