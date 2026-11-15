use crate::harness::{Harness, corpus, evidence, exchange, learn_set, uncertain};
use serde_json::{Value, json};
use uob_application::CommandDispatchOutcome;
use uob_contracts::*;

/// The corpus `set-mixed` request repeats one monitor to provoke `Duplicate`; the bridge refuses
/// to send that, so the five distinct monitors are used.
fn mixed() -> (Value, Value) {
    let mut request = corpus("set-mixed");
    let mut reply = corpus("set-reply-mixed");
    request["setMonitoringData"].as_array_mut().unwrap().pop();
    reply["setMonitoringResult"].as_array_mut().unwrap().pop();
    (request, reply)
}

#[tokio::test]
async fn set_monitors_keep_each_native_status_and_never_the_free_text() {
    let mut h = Harness::new().await;
    learn_set(&mut h, "10", "100000").await;
    let (request, reply) = mixed();
    let result = evidence(exchange(&mut h, "SetVariableMonitoring", request, reply).await);

    let MonitoringQuery201::SetVariableMonitoring { monitors } = &result.query else {
        panic!("query echo");
    };
    assert_eq!(monitors.len(), 5);
    assert_eq!(monitors[0].value.to_string(), "80");
    assert_eq!(monitors[0].severity.get(), 3);
    assert_eq!(monitors[0].monitor_type, MonitorType201::UpperThreshold);
    let MonitoringOutcome201::SetMonitors { monitors: answers } = &result.outcome else {
        panic!("set outcome");
    };
    let statuses: Vec<_> = answers.iter().map(|answer| answer.status).collect();
    assert_eq!(
        statuses,
        [
            SetMonitoringStatus201::Accepted,
            SetMonitoringStatus201::UnknownComponent,
            SetMonitoringStatus201::UnknownVariable,
            SetMonitoringStatus201::UnsupportedMonitorType,
            SetMonitoringStatus201::Rejected,
        ]
    );
    assert_eq!(answers[0].id, Some(4010));
    assert!(answers[1..].iter().all(|answer| answer.id.is_none()));
    assert_eq!(answers[4].reason_code.as_deref(), Some("ValueOutOfRange"));
    // `additionalInfo` is free station text and has no place in the evidence.
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("exceeds maxLimit")
    );
    // Mixed statuses are evidence, not acceptance.
    assert!(!result.accepted());
    h.close().await;
}

#[tokio::test]
async fn set_results_match_by_identity_in_any_order_and_echo_replacement_ids() {
    let mut h = Harness::new().await;
    learn_set(&mut h, "10", "100000").await;
    let reply = corpus("set-reply-accepted");
    let mut reversed = reply.clone();
    reversed["setMonitoringResult"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let result = evidence(
        exchange(
            &mut h,
            "SetVariableMonitoring",
            corpus("set-all-types"),
            reversed,
        )
        .await,
    );
    assert!(result.accepted());
    let MonitoringOutcome201::SetMonitors { monitors } = &result.outcome else {
        panic!("set outcome");
    };
    assert_eq!(monitors.len(), 5);
    assert_eq!(monitors[0].id, Some(4005));

    // A replacement keeps its id.
    let result = evidence(
        exchange(
            &mut h,
            "SetVariableMonitoring",
            corpus("set-replace-id"),
            corpus("set-reply-replaced"),
        )
        .await,
    );
    assert!(result.accepted());
    let MonitoringQuery201::SetVariableMonitoring { monitors } = &result.query else {
        panic!("query echo");
    };
    assert_eq!(monitors[0].id, Some(4001));
    let MonitoringOutcome201::SetMonitors { monitors } = &result.outcome else {
        panic!("set outcome");
    };
    assert_eq!(monitors[0].id, Some(4001));
    assert_eq!(monitors[0].status, SetMonitoringStatus201::Accepted);

    // A refused replacement carries no id and still yields evidence.
    let result = evidence(
        exchange(
            &mut h,
            "SetVariableMonitoring",
            corpus("set-replace-id"),
            corpus("set-reply-rejected-unknown-id"),
        )
        .await,
    );
    assert!(!result.accepted());
    let MonitoringOutcome201::SetMonitors { monitors } = &result.outcome else {
        panic!("set outcome");
    };
    assert_eq!(monitors[0].reason_code.as_deref(), Some("UnknownId"));
    h.close().await;
}

#[tokio::test]
async fn set_replies_that_do_not_correspond_to_the_request_are_never_success() {
    let mut h = Harness::new().await;
    let request = corpus("set-minimal");
    let good = corpus("set-reply-minimal");
    let result = &good["setMonitoringResult"][0];
    let mutate = |change: &dyn Fn(&mut Value)| {
        let mut reply = good.clone();
        change(&mut reply["setMonitoringResult"][0]);
        reply
    };
    // The unmodified reply is the baseline every mutation below departs from.
    let baseline = evidence(
        exchange(
            &mut h,
            "SetVariableMonitoring",
            request.clone(),
            good.clone(),
        )
        .await,
    );
    assert!(baseline.accepted());
    let replies = [
        // wrong count
        json!({"setMonitoringResult": [result, result]}),
        // a result for another component, variable, type or severity
        mutate(&|r| r["component"]["name"] = json!("Heater")),
        mutate(&|r| r["variable"]["name"] = json!("Humidity")),
        mutate(&|r| r["type"] = json!("Delta")),
        mutate(&|r| r["severity"] = json!(0)),
        // accepted without the id the charger assigned
        mutate(&|r| {
            r.as_object_mut().unwrap().remove("id");
        }),
        // control characters, an id beyond 32 bits, a severity beyond 9
        mutate(&|r| {
            r["statusInfo"] = json!({"reasonCode": "Busy", "additionalInfo": "bad\u{7}info"});
        }),
        mutate(&|r| r["id"] = json!(2_147_483_648_i64)),
        mutate(&|r| r["severity"] = json!(10)),
        // schema violations
        mutate(&|r| r["status"] = json!("NotFound")),
        mutate(&|r| r["unexpected"] = json!(true)),
        json!({"setMonitoringResult": []}),
        json!({}),
    ];
    for reply in replies {
        let outcome = exchange(
            &mut h,
            "SetVariableMonitoring",
            request.clone(),
            reply.clone(),
        )
        .await;
        assert!(uncertain(&outcome), "{reply} -> {outcome:?}");
    }

    // A replacement answered with a different id is not the replacement that was asked for.
    let mut other = corpus("set-reply-replaced");
    other["setMonitoringResult"][0]["id"] = json!(4999);
    let outcome = exchange(
        &mut h,
        "SetVariableMonitoring",
        corpus("set-replace-id"),
        other,
    )
    .await;
    assert!(uncertain(&outcome), "{outcome:?}");
    h.close().await;
}

#[tokio::test]
async fn clear_results_cover_exactly_the_requested_ids() {
    let mut h = Harness::new().await;
    h.learn("ClearVariableMonitoring", "ItemsPerMessage", "5")
        .await;
    h.learn("ClearVariableMonitoring", "BytesPerMessage", "100000")
        .await;
    let result = evidence(
        exchange(
            &mut h,
            "ClearVariableMonitoring",
            corpus("clear-multi"),
            corpus("clear-reply-multi-mixed"),
        )
        .await,
    );
    let MonitoringOutcome201::ClearMonitors { monitors } = &result.outcome else {
        panic!("clear outcome");
    };
    let statuses: Vec<_> = monitors.iter().map(|m| (m.id, m.status)).collect();
    assert_eq!(
        statuses,
        [
            (4001, ClearMonitoringStatus201::Accepted),
            (4002, ClearMonitoringStatus201::NotFound),
            (4003, ClearMonitoringStatus201::Rejected),
        ]
    );
    assert_eq!(monitors[2].reason_code.as_deref(), Some("HardWired"));
    assert!(!result.accepted());

    for (reply, accepted) in [
        ("clear-reply-accepted", true),
        ("clear-reply-not-found", false),
        ("clear-reply-rejected", false),
    ] {
        let result = evidence(
            exchange(
                &mut h,
                "ClearVariableMonitoring",
                corpus("clear-single"),
                corpus(reply),
            )
            .await,
        );
        assert_eq!(result.accepted(), accepted, "{reply}");
    }

    let multi = corpus("clear-multi");
    let replies = [
        json!({"clearMonitoringResult": [{"status": "Accepted", "id": 4001}, {"status": "Accepted", "id": 4777}, {"status": "Accepted", "id": 4003}]}),
        json!({"clearMonitoringResult": [{"status": "Accepted", "id": 4001}, {"status": "NotFound", "id": 4002}]}),
        json!({"clearMonitoringResult": [{"status": "Accepted", "id": 4001}, {"status": "Accepted", "id": 4001}, {"status": "NotFound", "id": 4003}]}),
        json!({"clearMonitoringResult": [{"status": "Accepted", "id": 4001}, {"status": "Accepted", "id": 4002}, {"status": "Accepted", "id": 4003}, {"status": "Accepted", "id": 4003}]}),
        json!({"clearMonitoringResult": [{"status": "Accepted", "id": 4001}, {"status": "NotFound", "id": 4002}, {"status": "Rejected", "id": 4_294_967_299_i64}]}),
        json!({"clearMonitoringResult": [{"status": "Accepted", "id": 4001}, {"status": "NotFound", "id": 4002}, {"status": "Rejected", "id": 4003, "statusInfo": {"reasonCode": "Bad\u{7}"}}]}),
    ];
    for reply in replies {
        let outcome = exchange(
            &mut h,
            "ClearVariableMonitoring",
            multi.clone(),
            reply.clone(),
        )
        .await;
        assert!(uncertain(&outcome), "{reply} -> {outcome:?}");
    }
    h.close().await;
}

#[tokio::test]
async fn base_and_level_keep_their_native_status() {
    let mut h = Harness::new().await;
    for (request, reply, status) in [
        (
            "base-all",
            "base-reply-accepted",
            DeviceReportAck201::Accepted,
        ),
        (
            "base-factory-default",
            "base-reply-rejected",
            DeviceReportAck201::Rejected,
        ),
        (
            "base-hard-wired-only",
            "base-reply-not-supported",
            DeviceReportAck201::NotSupported,
        ),
    ] {
        let result =
            evidence(exchange(&mut h, "SetMonitoringBase", corpus(request), corpus(reply)).await);
        assert_eq!(
            result.outcome,
            MonitoringOutcome201::Base { status },
            "{reply}"
        );
        assert_eq!(result.accepted(), status == DeviceReportAck201::Accepted);
    }
    for (request, reply, status) in [
        (
            "level-4",
            "level-reply-accepted",
            MonitoringLevelStatus201::Accepted,
        ),
        (
            "level-0",
            "level-reply-rejected",
            MonitoringLevelStatus201::Rejected,
        ),
    ] {
        let result =
            evidence(exchange(&mut h, "SetMonitoringLevel", corpus(request), corpus(reply)).await);
        assert_eq!(result.outcome, MonitoringOutcome201::Level { status });
        assert_eq!(
            result.query,
            MonitoringQuery201::SetMonitoringLevel {
                severity: MonitorSeverity201::new(
                    corpus(request)["severity"]
                        .as_u64()
                        .unwrap()
                        .try_into()
                        .unwrap()
                )
                .unwrap()
            }
        );
    }

    for (action, request, reply) in [
        (
            "SetMonitoringBase",
            corpus("base-all"),
            json!({"status": "Maybe"}),
        ),
        ("SetMonitoringBase", corpus("base-all"), json!({})),
        (
            "SetMonitoringLevel",
            corpus("level-4"),
            json!({"status": "NotSupported"}),
        ),
        (
            "SetMonitoringLevel",
            corpus("level-4"),
            json!({"status": "Accepted", "extra": 1}),
        ),
        (
            "SetMonitoringLevel",
            corpus("level-4"),
            json!({"status": "Accepted", "statusInfo": {"reasonCode": "x\u{1}"}}),
        ),
    ] {
        let outcome = exchange(&mut h, action, request, reply.clone()).await;
        assert!(uncertain(&outcome), "{reply} -> {outcome:?}");
    }
    h.close().await;
}

#[tokio::test]
async fn a_native_call_error_is_a_rejection_not_evidence() {
    let mut h = Harness::new().await;
    let id = h.id();
    let payload = corpus("level-4");
    let dispatched = h.dispatch(&id, "SetMonitoringLevel", payload.clone());
    h.call(&id, "SetMonitoringLevel", &payload).await;
    h.running
        .peer
        .send_text(json!([4, id, "NotSupported", "", {}]).to_string())
        .await
        .unwrap();
    assert!(matches!(
        dispatched.await.unwrap(),
        CommandDispatchOutcome::ProtocolResponse { accepted: false, error: Some(error) }
            if error.code == CommandErrorCode::ProtocolRejected
    ));
    h.close().await;
}
