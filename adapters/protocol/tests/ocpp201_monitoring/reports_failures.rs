use crate::harness::{Harness, corpus, evidence, negative};
use crate::reports::{ACTION, complete, failure, fragment, monitor, push, request};
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;
use uob_application::CommandDispatchOutcome;
use uob_contracts::*;

async fn accepted(h: &mut Harness, request_id: i64) {
    let id = h.id();
    request(
        h,
        &id,
        &json!({"requestId": request_id}),
        corpus("get-report-reply-accepted"),
    )
    .await;
}

#[tokio::test]
async fn gaps_duplicates_and_late_starts_fail_with_their_own_reason() {
    for (frames, reason, fragments) in [
        // seqNo 0, then 2
        (
            vec![(0, true), (2, false)],
            DeviceReportFailure201::OutOfOrderSequence,
            1,
        ),
        // the first part must be 0
        (
            vec![(1, true)],
            DeviceReportFailure201::OutOfOrderSequence,
            0,
        ),
        // seqNo 0 twice
        (
            vec![(0, true), (0, true)],
            DeviceReportFailure201::DuplicateSequence,
            1,
        ),
    ] {
        let mut h = Harness::new().await;
        accepted(&mut h, 7200).await;
        for (sequence, more) in frames {
            // Each fragment is well formed on its own and is acknowledged.
            push(
                &mut h,
                fragment(7200, sequence, more, vec![monitor("A", "B", 1)]),
            )
            .await;
        }
        let (_, report) = h.reports.merged().await;
        let (found, progress) = failure(&report);
        assert_eq!(found, reason);
        assert_eq!(progress.map(|p| p.fragments), Some(fragments));
        h.close().await;
    }
}

#[tokio::test]
async fn content_the_contract_cannot_carry_is_an_invalid_fragment() {
    let mut cases = vec![
        negative("report-severity-ten"),
        negative("report-id-out-of-int32"),
    ];
    // A control character in a name, and values without an exact plain-decimal representation.
    let mut named = fragment(7001, 0, false, vec![monitor("Te\u{7}mp", "B", 1)]);
    named["requestId"] = json!(7001);
    cases.push(named);
    for lexeme in [
        "1e999",
        "0.0000000000000000000000000000000000000000000000000000000000000000000001",
    ] {
        let mut text = fragment(7001, 0, false, vec![monitor("A", "B", 1)]).to_string();
        text = text.replace("\"value\":10", &format!("\"value\":{lexeme}"));
        cases.push(serde_json::from_str(&text).unwrap());
    }

    for payload in cases {
        let mut h = Harness::new().await;
        accepted(&mut h, 7001).await;
        let id = h.id();
        let answer = h
            .notify(&id, "NotifyMonitoringReport", payload.clone())
            .await;
        // The fragment itself is refused to the charger.
        assert_eq!(
            (answer[0].clone(), answer[1].clone()),
            (json!(4), json!(id)),
            "{payload}"
        );
        assert_eq!(answer[2], "ProtocolError");
        let (_, report) = h.reports.merged().await;
        assert_eq!(
            failure(&report).0,
            DeviceReportFailure201::InvalidFragment,
            "{payload}"
        );
        h.close().await;
    }
}

#[tokio::test]
async fn a_schema_invalid_fragment_for_a_collected_report_fails_it() {
    let mut h = Harness::new().await;
    accepted(&mut h, 7001).await;
    let mut payload = fragment(7001, 0, true, vec![monitor("A", "B", 1)]);
    payload["unexpected"] = json!(true);
    let id = h.id();
    let answer = h.notify(&id, "NotifyMonitoringReport", payload).await;
    assert_eq!(answer[0], 4);
    let (_, report) = h.reports.merged().await;
    assert_eq!(failure(&report).0, DeviceReportFailure201::InvalidFragment);
    h.close().await;
}

#[tokio::test]
async fn session_replacement_mid_report_ends_with_disconnected_and_its_progress() {
    let mut h = Harness::new().await;
    accepted(&mut h, 7300).await;
    push(&mut h, fragment(7300, 0, true, vec![monitor("A", "B", 1)])).await;
    push(&mut h, fragment(7300, 1, true, vec![monitor("A", "C", 2)])).await;
    h.remote.detach_device_model();

    let (_, report) = h.reports.merged().await;
    let (reason, progress) = failure(&report);
    assert_eq!(reason, DeviceReportFailure201::Disconnected);
    assert_eq!(progress.map(|p| (p.fragments, p.items)), Some((2, 2)));
    h.close().await;
}

#[tokio::test]
async fn teardown_before_any_fragment_is_disconnected_without_progress() {
    let mut h = Harness::new().await;
    accepted(&mut h, 7301).await;
    h.remote.detach_device_model();

    let (_, report) = h.reports.merged().await;
    let (reason, progress) = failure(&report);
    assert_eq!(reason, DeviceReportFailure201::Disconnected);
    assert_eq!(
        progress.map(|p| (p.fragments, p.items, p.bytes)),
        Some((0, 0, 0))
    );
    h.close().await;
}

#[tokio::test]
async fn origin_cancellation_does_not_abandon_the_collection() {
    let mut h = Harness::new().await;
    let id = h.id();
    let payload = json!({"requestId": 7400});
    let dispatched = h.dispatch(&id, ACTION, payload.clone());
    h.call(&id, ACTION, &payload).await;
    h.reply(&id, corpus("get-report-reply-accepted")).await;
    let outcome = dispatched.await.unwrap();
    assert!(matches!(
        outcome,
        CommandDispatchOutcome::MonitoringResponse201(_)
    ));
    // The response future is long gone; the exchange owns the collection.
    drop(outcome);
    push(&mut h, fragment(7400, 0, false, vec![monitor("A", "B", 1)])).await;
    let (_, report) = h.reports.merged().await;
    assert_eq!(complete(&report).1.len(), 1);
    h.close().await;
}

#[tokio::test]
async fn completion_does_not_depend_on_the_application_recording_the_acknowledgement() {
    let mut h = Harness::new().await;
    accepted(&mut h, 7500).await;
    // The application never writes in this test, so the collector alone must finish the report.
    push(&mut h, fragment(7500, 0, false, vec![monitor("A", "B", 1)])).await;
    let (_, report) = h.reports.merged().await;
    assert_eq!(complete(&report).1.len(), 1);

    // Storage keeps no row-state for the collector to wait on: one attempt, however long the
    // application write is delayed.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(h.reports.attempts(), 1);
    let [finished] = h.reports.finished().try_into().ok().expect("one finish");
    assert_eq!(finished.request.as_str(), "mon-0001");
    assert_eq!(
        finished.evidence.query,
        MonitoringQuery201::GetMonitoringReport {
            request_id: 7500,
            criteria: vec![],
            component_variables: vec![]
        }
    );
    let MonitoringOutcome201::Report { ack, .. } = finished.evidence.outcome else {
        panic!("report outcome");
    };
    assert_eq!(ack, Some(DeviceReportAck201::Accepted));
    assert_eq!(
        finished.lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None
        }
    );
    h.close().await;
}

#[tokio::test]
async fn an_unknown_request_is_not_retried() {
    let mut h = Harness::new().await;
    h.reports.unknown();
    accepted(&mut h, 7502).await;
    push(&mut h, fragment(7502, 0, false, vec![monitor("A", "B", 1)])).await;

    timeout(Duration::from_secs(5), async {
        while h.reports.attempts() < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the one finish attempt");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.reports.attempts(), 1);
    assert!(h.reports.recorded().is_empty());
    h.close().await;
}

#[tokio::test]
async fn a_failing_store_is_tried_once_more_with_a_storage_failure_and_never_loops() {
    let mut h = Harness::new().await;
    h.reports.fail();
    accepted(&mut h, 7501).await;
    push(&mut h, fragment(7501, 0, false, vec![monitor("A", "B", 1)])).await;

    timeout(Duration::from_secs(5), async {
        while h.reports.attempts() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("complete attempt and its storage-failure fallback");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.reports.attempts(), 2);
    assert!(h.reports.recorded().is_empty());
    h.close().await;
}

#[tokio::test]
async fn an_unbound_session_supports_no_report_and_answers_fragments_not_implemented() {
    let mut h = Harness::unbound().await;
    let id = h.id();
    let outcome = h
        .dispatch(&id, ACTION, corpus("get-report-no-filter"))
        .await
        .unwrap();
    assert!(
        matches!(outcome, CommandDispatchOutcome::NotTransmitted { error }
        if error.code == CommandErrorCode::UnsupportedOperation)
    );
    h.no_extra_call().await;

    let id = h.id();
    let answer = h
        .notify(&id, "NotifyMonitoringReport", corpus("report-single"))
        .await;
    assert_eq!(
        (&answer[0], &answer[1], &answer[2]),
        (&json!(4), &json!(id), &json!("NotImplemented"))
    );

    // The report-free actions need no store.
    let id = h.id();
    let payload = corpus("level-4");
    let dispatched = h.dispatch(&id, "SetMonitoringLevel", payload.clone());
    h.call(&id, "SetMonitoringLevel", &payload).await;
    h.reply(&id, corpus("level-reply-accepted")).await;
    assert!(evidence(dispatched.await.unwrap()).accepted());
    h.close().await;
}

#[tokio::test]
async fn an_unanswered_call_is_uncertain_and_collects_nothing() {
    let mut h = Harness::new().await;
    let id = h.id();
    let payload = json!({"requestId": 7600});
    let dispatched = h.dispatch(&id, ACTION, payload.clone());
    h.call(&id, ACTION, &payload).await;
    let outcome = dispatched.await.unwrap();
    assert!(matches!(
        outcome,
        CommandDispatchOutcome::TransmissionUncertain { .. }
    ));
    assert_eq!(h.reports.attempts(), 0);
    h.close().await;
}
