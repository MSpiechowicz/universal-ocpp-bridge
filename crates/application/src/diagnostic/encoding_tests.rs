use std::sync::Arc;

use time::OffsetDateTime;
use uob_contracts::{
    AvailabilityState, ProcessInstanceId, StationId, TraceDirection, TraceId, TraceRecord,
    TraceSequence, TraceStage, UtcTimestamp,
};

use super::{
    DiagnosticAttribute, DiagnosticBoundary, DiagnosticObservation, DiagnosticOutcome,
    DiagnosticSummary, DiagnosticTraceContext, MAX_DIAGNOSTIC_RECORD_BYTES, SafeDiagnosticField,
    SanitizedDiagnostic, SensitiveDataClass, SensitiveDiagnosticValue, UnknownVendorPayload,
};

fn context() -> DiagnosticTraceContext {
    DiagnosticTraceContext {
        trace_id: TraceId::new("bounded-trace").unwrap(),
        process_instance_id: ProcessInstanceId::new("bounded-process").unwrap(),
        trace_sequence: TraceSequence(1),
        target: None,
        correlation_id: None,
        parent_trace_id: None,
        stage: TraceStage::new("validation").unwrap(),
        direction: TraceDirection::Inbound,
        observed_at: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
        duration_micros: None,
        outcome: DiagnosticOutcome::Succeeded,
    }
}

fn observation(attributes: Vec<DiagnosticAttribute>) -> DiagnosticObservation {
    DiagnosticObservation {
        summary: DiagnosticSummary::PayloadObserved,
        attributes,
    }
}

fn station(value: &str) -> DiagnosticAttribute {
    DiagnosticAttribute::Safe(SafeDiagnosticField::Station(StationId::new(value).unwrap()))
}

fn decoded(sanitized: &SanitizedDiagnostic) -> TraceRecord {
    assert!(sanitized.encoded_json().len() <= MAX_DIAGNOSTIC_RECORD_BYTES);
    serde_json::from_slice(sanitized.encoded_json()).expect("complete valid trace JSON")
}

#[test]
fn oversized_safe_details_are_omitted_with_source_byte_counts() {
    for value in ["x".repeat(128 * 1024), "λ😀".repeat(32 * 1024)] {
        let sanitized = DiagnosticBoundary
            .serialize(
                context(),
                observation(vec![
                    station(&value),
                    DiagnosticAttribute::Safe(SafeDiagnosticField::PayloadBytes(42)),
                ]),
            )
            .unwrap();
        let details = decoded(&sanitized).redacted_details.unwrap();
        assert!(details.truncated);
        assert!(!details.fields.contains_key("station_id"));
        assert_eq!(details.fields["payload_bytes"], "42");
        assert_eq!(details.fields["details.omitted_fields"], "1");
        assert_eq!(
            details.fields["details.original_size"],
            (value.len() + 2).to_string()
        );
        assert_eq!(sanitized.audit().exposed_fields(), &["payload_bytes"]);
    }
}

#[test]
fn encoded_limit_counts_json_escaping_and_preserves_utf8() {
    // Raw UTF-8 fits, but escaped quotes and controls push the JSON beyond the record limit.
    let escaped = "\"\u{1}λ😀".repeat(5000);
    assert!(escaped.len() < MAX_DIAGNOSTIC_RECORD_BYTES);
    let sanitized = DiagnosticBoundary
        .serialize(context(), observation(vec![station(&escaped)]))
        .unwrap();
    let details = decoded(&sanitized).redacted_details.unwrap();
    assert!(details.truncated);
    assert_eq!(
        details.fields["details.original_size"],
        escaped.len().to_string()
    );
    assert!(!details.fields.contains_key("station_id"));

    let unicode = "😀".repeat(15_000);
    let sanitized = DiagnosticBoundary
        .serialize(context(), observation(vec![station(&unicode)]))
        .unwrap();
    let details = decoded(&sanitized).redacted_details.unwrap();
    assert!(!details.truncated);
    assert_eq!(details.fields["station_id"], unicode);
}

#[test]
fn metadata_and_audit_share_the_complete_record_budget() {
    let mut large_context = context();
    large_context.process_instance_id =
        ProcessInstanceId::new("p".repeat(MAX_DIAGNOSTIC_RECORD_BYTES - 1500)).unwrap();
    let sanitized = DiagnosticBoundary
        .serialize(large_context, observation(vec![station(&"s".repeat(2000))]))
        .unwrap();
    let details = decoded(&sanitized).redacted_details.unwrap();
    assert!(details.truncated);
    assert_eq!(details.fields["details.omitted_fields"], "1");
    assert!(!details.fields.contains_key("station_id"));
}

#[test]
fn required_metadata_that_cannot_fit_fails_closed() {
    for value in ["p".repeat(MAX_DIAGNOSTIC_RECORD_BYTES), "\"".repeat(40_000)] {
        let mut oversized_context = context();
        oversized_context.process_instance_id = ProcessInstanceId::new(value).unwrap();
        assert!(
            DiagnosticBoundary
                .serialize(oversized_context, observation(Vec::new()))
                .is_err()
        );
    }
}

#[test]
fn vendor_source_sizes_survive_omission_without_source_content() {
    let secret = "never-disclose-this-vendor-secret".repeat(32 * 1024);
    let sanitized = DiagnosticBoundary
        .serialize(
            context(),
            observation(vec![
                DiagnosticAttribute::UnknownVendorPayload(UnknownVendorPayload::new(
                    secret.as_bytes(),
                )),
                DiagnosticAttribute::UnknownVendorPayload(UnknownVendorPayload::new(&b"other"[..])),
            ]),
        )
        .unwrap();
    let details = decoded(&sanitized).redacted_details.unwrap();
    assert!(details.truncated);
    assert_eq!(
        details.fields["vendor_payload.original_size"],
        (secret.len() + 5).to_string()
    );
    assert_eq!(
        details.fields["details.original_size"],
        (secret.len() + 5).to_string()
    );
    assert_eq!(sanitized.audit().omitted_unknown_vendor_payloads(), 2);
    assert!(!String::from_utf8_lossy(sanitized.encoded_json()).contains("never-disclose"));
}

#[test]
fn many_fields_bound_audit_and_clones_share_both_allocations() {
    let mut attributes: Vec<_> = (0..20_000)
        .map(|index| {
            DiagnosticAttribute::Safe(SafeDiagnosticField::AvailabilityChange {
                index,
                before: AvailabilityState::Available,
                after: AvailabilityState::Unavailable,
            })
        })
        .collect();
    attributes.extend((0..2000).map(|_| {
        DiagnosticAttribute::Sensitive(SensitiveDiagnosticValue::new(
            SensitiveDataClass::Credential,
            "repeated-secret",
        ))
    }));
    let sanitized = DiagnosticBoundary
        .serialize(context(), observation(attributes))
        .unwrap();
    let details = decoded(&sanitized).redacted_details.unwrap();
    assert_eq!(sanitized.audit().exposed_fields().len(), 64);
    assert_eq!(sanitized.audit().redacted_classes(), &["credential"]);
    assert_eq!(details.fields["details.omitted_fields"], "19936");
    let cloned = sanitized.clone();
    assert!(Arc::ptr_eq(&sanitized.encoded_json, &cloned.encoded_json));
    assert!(Arc::ptr_eq(&sanitized.audit, &cloned.audit));
}

#[test]
fn duplicate_safe_names_keep_the_latest_value_without_expanding_the_audit() {
    let attributes = (0..2000)
        .map(|value| DiagnosticAttribute::Safe(SafeDiagnosticField::PayloadBytes(value)))
        .collect();
    let sanitized = DiagnosticBoundary
        .serialize(context(), observation(attributes))
        .unwrap();
    let details = decoded(&sanitized).redacted_details.unwrap();
    assert!(!details.truncated);
    assert_eq!(details.fields["payload_bytes"], "1999");
    assert_eq!(sanitized.audit().exposed_fields(), &["payload_bytes"]);
}

#[test]
fn producer_detail_shedding_marks_the_standalone_record_truncated() {
    let sanitized = DiagnosticBoundary
        .serialize(
            context(),
            observation(vec![DiagnosticAttribute::Safe(
                SafeDiagnosticField::StateDetailsOmitted,
            )]),
        )
        .unwrap();
    let details = decoded(&sanitized).redacted_details.unwrap();
    assert!(details.truncated);
    assert_eq!(details.fields["state_details_omitted"], "true");
    assert_eq!(details.fields["details.omitted_fields"], "0");
}
