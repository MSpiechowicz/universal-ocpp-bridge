use uob_contracts::{TraceDirection, TraceOutcome, TraceRecord};

const FIXTURE: &str = include_str!("fixtures/trace-record-v1.json");

#[test]
fn diagnostic_fixture_retains_required_correlation_and_target_metadata() {
    let trace: TraceRecord = serde_json::from_str(FIXTURE).expect("decode trace fixture");

    assert_eq!(trace.process_instance_id.as_str(), "process-production-7");
    assert_eq!(trace.trace_sequence.0, 9001);
    assert_eq!(trace.trace_id.as_str(), "trace-9001");
    assert_eq!(
        trace.parent_trace_id.expect("parent trace").as_str(),
        "trace-9000"
    );
    assert_eq!(trace.direction, TraceDirection::Outbound);
    assert!(matches!(trace.outcome, TraceOutcome::Succeeded));
    let target = trace.target.expect("target identity");
    assert_eq!(target.instance_id.as_str(), "main-ems");
    assert_eq!(target.kind.as_str(), "ems-scada.http");
    let details = trace.redacted_details.expect("redacted details");
    assert_eq!(details.fields["payload"], "[REDACTED]");
    assert!(details.truncated);
}

#[test]
fn diagnostics_are_tolerant_of_additive_optional_fields() {
    let value = FIXTURE.replace(
        "\n  \"redacted_details\"",
        "\n  \"future_optional_diagnostic\": true,\n  \"redacted_details\"",
    );

    serde_json::from_str::<TraceRecord>(&value).expect("v1 reader ignores additive trace field");
}
