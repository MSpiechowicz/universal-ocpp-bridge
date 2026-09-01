use serde::Deserialize;
use serde_json::{Value, json};
use uob_contracts::{
    ExportBatch, ExportBatchError, ExportErrorCode, ExportOutcome, ExportRecordKind,
    ExportRecordOutcome, ExportRecordOutcomeKind, ExportReport, ExportReportError,
    ExportUncertainStage,
};

fn batch_fixture() -> ExportBatch {
    serde_json::from_str(include_str!("fixtures/export-batch-v1.json"))
        .expect("valid privacy-filtered export batch")
}

fn error_code(value: &str) -> ExportErrorCode {
    ExportErrorCode::new(value).expect("valid sanitized error code")
}

#[test]
fn serialization_fixture_preserves_measurement_semantics_and_child_identities_across_retries() {
    let first_attempt = batch_fixture();
    let retry_attempt = batch_fixture();
    let identities = first_attempt
        .records()
        .iter()
        .map(|record| record.metadata().identity.clone())
        .collect::<Vec<_>>();

    assert_eq!(identities[0].record_id, identities[1].record_id);
    assert_eq!(
        identities[0].subrecord_id.expect("first child").ordinal(),
        0
    );
    assert_eq!(
        identities[1].subrecord_id.expect("second child").ordinal(),
        1
    );
    assert_eq!(
        identities,
        retry_attempt
            .records()
            .iter()
            .map(|record| record.metadata().identity.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        first_attempt.records()[0].kind(),
        ExportRecordKind::Measurement
    );
    assert_eq!(
        first_attempt.records()[1].kind(),
        ExportRecordKind::PointChange
    );

    let encoded = serde_json::to_value(&first_attempt).expect("encode export batch");
    let original: Value =
        serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).expect("JSON fixture");
    assert_eq!(encoded, original);
    assert_eq!(
        encoded["records"][0]["payload"]["data"]["value"]["value"],
        "12.34567890123456789"
    );
    let measurement = &encoded["records"][0]["payload"]["data"]["measurement"];
    assert_eq!(measurement["original_unit"], "W");
    assert_eq!(measurement["original_value"], "12345.678901234567890");
    assert_eq!(measurement["phase"], "L1-N");
    assert_eq!(measurement["context"], "sample.periodic");
    assert_eq!(
        encoded["records"][0]["payload"]["data"]["quality"]["level"],
        "uncertain"
    );
}

#[derive(Deserialize)]
struct ForbiddenFieldFixture {
    field: String,
    location: String,
    value: String,
}

#[test]
fn privacy_fixtures_cannot_enter_the_batch_receiver() {
    let forbidden: Vec<ForbiddenFieldFixture> =
        serde_json::from_str(include_str!("fixtures/export-privacy-forbidden-v1.json"))
            .expect("privacy fixtures");

    for fixture in forbidden {
        let mut batch: Value = serde_json::from_str(include_str!("fixtures/export-batch-v1.json"))
            .expect("batch JSON");
        let target = match fixture.location.as_str() {
            "batch" => batch.as_object_mut().expect("batch object"),
            "record" => batch["records"][0].as_object_mut().expect("record object"),
            "payload" => batch["records"][0]["payload"]
                .as_object_mut()
                .expect("payload object"),
            location => panic!("unknown privacy fixture location {location}"),
        };
        target.insert(fixture.field.clone(), Value::String(fixture.value));

        assert!(
            serde_json::from_value::<ExportBatch>(batch).is_err(),
            "forbidden field {} must be rejected before buffering",
            fixture.field
        );
    }

    let mut unsupported_payload: Value =
        serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).expect("batch JSON");
    unsupported_payload["records"][0]["payload"] = json!({
        "kind": "payment",
        "data": { "payment_token": "secret" }
    });
    assert!(serde_json::from_value::<ExportBatch>(unsupported_payload).is_err());
}

#[test]
fn duplicate_root_and_subrecord_identity_is_rejected_before_delivery() {
    let batch = batch_fixture();
    let records = vec![batch.records()[0].clone(), batch.records()[0].clone()];
    let result = ExportBatch::new(
        batch.batch_id().clone(),
        batch.destination().clone(),
        records,
    );

    assert!(matches!(result, Err(ExportBatchError::DuplicateRecord(_))));
}

#[test]
fn bytes_written_and_unknown_commit_never_advance_a_checkpoint() {
    let batch = batch_fixture();

    for stage in [
        ExportUncertainStage::BytesWritten,
        ExportUncertainStage::CommitOutcomeUnknown,
    ] {
        let report = ExportReport::for_batch(
            &batch,
            ExportOutcome::Uncertain {
                stage,
                error: error_code("commit_unconfirmed"),
            },
        )
        .expect("valid uncertain report");
        assert!(!report.is_fully_committed());
        assert_eq!(report.confirmed_record_ids().count(), 0);
    }

    let retryable = ExportReport::for_batch(
        &batch,
        ExportOutcome::Retryable {
            error: error_code("connection_lost"),
        },
    )
    .expect("valid retryable report");
    assert_eq!(retryable.confirmed_record_ids().count(), 0);
}

#[test]
fn only_confirmed_atomic_or_explicit_per_record_commits_are_checkpoint_evidence() {
    let batch = batch_fixture();
    let committed =
        ExportReport::for_batch(&batch, ExportOutcome::Committed).expect("valid committed report");
    assert!(committed.is_fully_committed());
    assert_eq!(committed.confirmed_record_ids().count(), 2);

    let first = batch.records()[0].metadata().identity.clone();
    let second = batch.records()[1].metadata().identity.clone();
    let partial = ExportReport::for_batch(
        &batch,
        ExportOutcome::Partial {
            records: vec![
                ExportRecordOutcome {
                    identity: first.clone(),
                    result: ExportRecordOutcomeKind::Committed,
                },
                ExportRecordOutcome {
                    identity: second,
                    result: ExportRecordOutcomeKind::Permanent {
                        error: error_code("unsupported_record"),
                    },
                },
            ],
        },
    )
    .expect("complete partial report");
    assert!(!partial.is_fully_committed());
    assert_eq!(
        partial.confirmed_record_ids().cloned().collect::<Vec<_>>(),
        vec![first]
    );
}

#[test]
fn partial_results_must_classify_every_record_exactly_once() {
    let batch = batch_fixture();
    let first = batch.records()[0].metadata().identity.clone();
    let missing = ExportReport::for_batch(
        &batch,
        ExportOutcome::Partial {
            records: vec![ExportRecordOutcome {
                identity: first.clone(),
                result: ExportRecordOutcomeKind::Committed,
            }],
        },
    );
    assert_eq!(
        missing,
        Err(ExportReportError::IncompletePartialClassification)
    );

    let duplicate = ExportReport::for_batch(
        &batch,
        ExportOutcome::Partial {
            records: vec![
                ExportRecordOutcome {
                    identity: first.clone(),
                    result: ExportRecordOutcomeKind::Committed,
                },
                ExportRecordOutcome {
                    identity: first,
                    result: ExportRecordOutcomeKind::Retryable {
                        error: error_code("retry"),
                    },
                },
            ],
        },
    );
    assert_eq!(duplicate, Err(ExportReportError::DuplicatePartialIdentity));
}

#[test]
fn reports_remain_bound_to_the_original_batch_and_destination_revision() {
    let batch = batch_fixture();
    let report =
        ExportReport::for_batch(&batch, ExportOutcome::Committed).expect("valid committed report");

    assert_eq!(report.batch_id(), batch.batch_id());
    assert_eq!(report.destination(), batch.destination());
    assert_eq!(
        report.record_ids(),
        batch
            .records()
            .iter()
            .map(|record| record.metadata().identity.clone())
            .collect::<Vec<_>>()
    );
}
