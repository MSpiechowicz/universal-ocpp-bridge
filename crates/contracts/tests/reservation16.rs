use serde_json::{Value, json};
use uob_contracts::*;

fn request(id: &Value, connector: &Value) -> Value {
    json!({"connectorId":connector,"expiryDate":"2099-01-01T00:00:00Z","reservationId":id,"reservationReference":format!("reserve16:{}", "a".repeat(64))})
}
#[test]
fn exact_protected_wrapper_preserves_full_signed_ids_and_native_connector_bounds() {
    let schema: Value = serde_json::from_str(include_str!(
        "../schemas/v1.0/reserve-now-reference-16.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    for id in [i32::MIN, -1, 0, 1, i32::MAX] {
        for connector in [0, 1, u32::MAX] {
            let value = request(&json!(id), &json!(connector));
            assert!(validator.is_valid(&value));
            let decoded: ReserveNowReference16 = serde_json::from_value(value).unwrap();
            assert_eq!(decoded.reservation_id, id);
            assert_eq!(decoded.connector_id, connector);
        }
    }
    for value in [
        request(&json!(2_147_483_648_u64), &json!(1)),
        request(&json!(-2_147_483_649_i64), &json!(1)),
        request(&json!(1), &json!(-1)),
        request(&json!(1), &json!(4_294_967_296_u64)),
    ] {
        assert!(!validator.is_valid(&value));
        assert!(serde_json::from_value::<ReserveNowReference16>(value).is_err());
    }
    for field in ["idTag", "parentIdTag", "extra"] {
        let mut value = request(&json!(0), &json!(1));
        value[field] = json!("PRIVATE-RESERVATION");
        assert!(!validator.is_valid(&value));
        assert!(serde_json::from_value::<ReserveNowReference16>(value).is_err());
    }
}
#[test]
fn old_results_remain_readable_and_all_native_statuses_survive_nested_exports() {
    let old: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let mut result: CommandResult = serde_json::from_value(old[1].clone()).unwrap();
    assert!(result.reservation_16.is_none());
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/v1.11/command-result.schema.json")).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let export_schema: Value =
        serde_json::from_str(include_str!("../schemas/v1.12/export-record.schema.json")).unwrap();
    let export_validator = jsonschema::draft202012::new(&export_schema).unwrap();
    let statuses = [
        ReserveNowStatus16::Accepted,
        ReserveNowStatus16::Faulted,
        ReserveNowStatus16::Occupied,
        ReserveNowStatus16::Rejected,
        ReserveNowStatus16::Unavailable,
    ];
    for status in statuses {
        let evidence = ReservationResult16::ReserveNow {
            reservation_id: i32::MIN,
            connector_id: 0,
            status: Some(status),
            reconciliation: ReservationReconciliation16 {
                revision: 7,
                state: ReservationState16::Consumed,
                observed_at: result.recorded_at,
                source_time: Some(result.recorded_at),
            },
        };
        result.schema_version = ContractVersion::V1_RESERVATION_16;
        result.lifecycle = CommandLifecycle::ProtocolResponse {
            accepted: evidence.accepted(),
            error: None,
        };
        result.reservation_16 = Some(evidence.clone());
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(validator.is_valid(&encoded));
        assert_eq!(
            serde_json::from_value::<CommandResult>(encoded)
                .unwrap()
                .reservation_16,
            Some(evidence)
        );
        let batch: ExportBatch =
            serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).unwrap();
        let record = ExportRecord::new(
            batch.records()[0].metadata().clone(),
            ExportPayload::CommandResult(result.clone()),
        );
        assert_eq!(record.metadata().schema_version.revision, 13);
        assert!(export_validator.is_valid(&serde_json::to_value(record).unwrap()));
    }
    for status in [
        CancelReservationStatus16::Accepted,
        CancelReservationStatus16::Rejected,
    ] {
        let value = json!({"action":"CancelReservation","reservation_id":0,"status":status,"reconciliation":{"revision":8,"state":"cancelled","observed_at":result.recorded_at}});
        let evidence: ReservationResult16 = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            evidence.accepted(),
            status == CancelReservationStatus16::Accepted
        );
        for field in [
            "idTag",
            "parentIdTag",
            "token_key",
            "group_key",
            "reservationReference",
        ] {
            let mut private = value.clone();
            private[field] = json!("PRIVATE-MARKER");
            assert!(serde_json::from_value::<ReservationResult16>(private).is_err());
        }
    }
}
