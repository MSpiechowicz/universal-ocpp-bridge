use std::sync::Arc;

use time::OffsetDateTime;
use uob_contracts::{
    CorrelationId, ProcessInstanceId, ProtocolActionName, StationId, TraceDirection, TraceId,
    TraceRecord, TraceSequence, TraceStage, UtcTimestamp,
};

use super::{
    DiagnosticAttribute, DiagnosticBoundary, DiagnosticObservation, DiagnosticOutcome,
    DiagnosticSummary, DiagnosticTraceContext, SafeDiagnosticField, SafeEndpointLabel,
    SafeEndpointLabelError, SensitiveDataClass, SensitiveDiagnosticValue, UnknownVendorPayload,
};

fn context() -> DiagnosticTraceContext {
    DiagnosticTraceContext {
        trace_id: TraceId::new("trace-redaction-1").expect("trace ID"),
        process_instance_id: ProcessInstanceId::new("process-redaction-1").expect("process ID"),
        trace_sequence: TraceSequence(1),
        target: None,
        correlation_id: Some(CorrelationId::new("correlation-1").expect("correlation ID")),
        parent_trace_id: None,
        stage: TraceStage::new("protocol.receive").expect("stage"),
        direction: TraceDirection::Inbound,
        observed_at: UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH),
        duration_micros: Some(42),
        outcome: DiagnosticOutcome::Succeeded,
    }
}

#[test]
fn one_serialization_redacts_every_sensitive_class_for_all_sinks() {
    let seeds = [
        (SensitiveDataClass::AuthorizationToken, "seed-bearer-token"),
        (SensitiveDataClass::Credential, "seed-client-secret"),
        (SensitiveDataClass::EndpointSecret, "seed-uri-password"),
        (SensitiveDataClass::PaymentSensitive, "seed-card-data"),
    ];
    let mut attributes = vec![
        DiagnosticAttribute::Safe(SafeDiagnosticField::Action(
            ProtocolActionName::new("BootNotification").expect("action"),
        )),
        DiagnosticAttribute::Safe(SafeDiagnosticField::Station(
            StationId::new("station-7").expect("station ID"),
        )),
        DiagnosticAttribute::Safe(SafeDiagnosticField::EndpointLabel(
            SafeEndpointLabel::new("primary-csms").expect("safe label"),
        )),
    ];
    attributes.extend(seeds.iter().map(|(class, value)| {
        DiagnosticAttribute::Sensitive(SensitiveDiagnosticValue::new(*class, *value))
    }));

    let sanitized = DiagnosticBoundary
        .serialize(
            context(),
            DiagnosticObservation {
                summary: DiagnosticSummary::PayloadObserved,
                attributes,
            },
        )
        .expect("sanitized serialization");
    let log_sink = sanitized.clone();
    let trace_ring_sink = sanitized.clone();
    let broadcast_sink = sanitized.clone();
    let export_sink = sanitized.clone();
    let api_error_sink = sanitized.clone();

    for sink in [
        &log_sink,
        &trace_ring_sink,
        &broadcast_sink,
        &export_sink,
        &api_error_sink,
    ] {
        let rendered = String::from_utf8_lossy(sink.encoded_json());
        for (_, seed) in seeds {
            assert!(!rendered.contains(seed));
        }
        assert!(rendered.contains("[REDACTED]"));
        assert!(Arc::ptr_eq(&sanitized.encoded_json, &sink.encoded_json));
    }
}

#[test]
fn unknown_and_renamed_vendor_content_fails_closed_with_audit_evidence() {
    let secret = "renamed_vendor_credential=do-not-expose";
    let sanitized = DiagnosticBoundary
        .serialize(
            context(),
            DiagnosticObservation {
                summary: DiagnosticSummary::ValidationRejected,
                attributes: vec![DiagnosticAttribute::UnknownVendorPayload(
                    UnknownVendorPayload::new(secret.as_bytes()),
                )],
            },
        )
        .expect("sanitized serialization");

    let rendered = String::from_utf8_lossy(sanitized.encoded_json());
    assert!(!rendered.contains(secret));
    assert!(rendered.contains("[OMITTED: unknown_vendor_payload]"));
    assert_eq!(sanitized.audit().omitted_unknown_vendor_payloads(), 1);

    let trace: TraceRecord =
        serde_json::from_slice(sanitized.encoded_json()).expect("trace contract");
    let details = trace.redacted_details.expect("redacted details");
    assert!(details.truncated);
    assert_eq!(details.fields["audit.omitted_unknown_vendor_payloads"], "1");
}

#[test]
fn disclosure_audit_names_only_safe_fields_and_sensitive_classes() {
    let sanitized = DiagnosticBoundary
        .serialize(
            context(),
            DiagnosticObservation {
                summary: DiagnosticSummary::OperationCompleted,
                attributes: vec![
                    DiagnosticAttribute::Safe(SafeDiagnosticField::PayloadBytes(1024)),
                    DiagnosticAttribute::Sensitive(SensitiveDiagnosticValue::new(
                        SensitiveDataClass::Credential,
                        "database-password",
                    )),
                ],
            },
        )
        .expect("sanitized serialization");

    assert_eq!(sanitized.audit().exposed_fields(), &["payload_bytes"]);
    assert_eq!(sanitized.audit().redacted_classes(), &["credential"]);
    assert!(!format!("{sanitized:?}").contains("database-password"));
}

#[test]
fn endpoint_labels_cannot_accept_addresses_or_embedded_credentials() {
    for unsafe_value in [
        "https://user:secret@example.invalid/path?token=seed",
        "user@example.invalid",
        "example.invalid:443",
        "target?api_key=seed",
    ] {
        assert_eq!(
            SafeEndpointLabel::new(unsafe_value),
            Err(SafeEndpointLabelError::AddressOrCredentialMaterial)
        );
    }
    assert_eq!(
        SafeEndpointLabel::new("primary-export")
            .expect("safe label")
            .as_str(),
        "primary-export"
    );
}
