use serde_json::{Value, json};
use uob_contracts::*;

fn request(id: &Value, evse: Option<&Value>) -> Value {
    let mut value = json!({"id":id,"expiryDateTime":"2099-01-01T00:00:00Z","reservationReference":format!("reserve201:{}", "a".repeat(64))});
    if let Some(evse) = evse {
        value["evseId"] = evse.clone();
    }
    value
}
fn validator(path: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(path).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
}

#[test]
fn exact_protected_wrapper_preserves_signed_ids_evse_scope_and_connector_type() {
    let validator = validator(include_str!(
        "../schemas/v1.0/reserve-now-reference-201.schema.json"
    ));
    for id in [i32::MIN, -1, 0, 1, i32::MAX] {
        for evse in [None, Some(json!(1)), Some(json!(i32::MAX))] {
            let value = request(&json!(id), evse.as_ref());
            assert!(validator.is_valid(&value));
            let decoded: ReserveNowReference201 = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(decoded.id, id);
            assert_eq!(
                decoded.evse_id,
                evse.and_then(|e| e.as_u64())
                    .map(|e| u32::try_from(e).unwrap())
            );
            assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
        }
    }
    let mut typed = request(&json!(5), None);
    typed["connectorType"] = json!("cCCS2");
    let decoded: ReserveNowReference201 = serde_json::from_value(typed.clone()).unwrap();
    assert_eq!(
        decoded.connector_type,
        Some(ReservationConnectorType201::CCcs2)
    );
    assert!(validator.is_valid(&typed));
    for value in [
        request(&json!(2_147_483_648_u64), None),
        request(&json!(-2_147_483_649_i64), None),
        request(&json!(1), Some(&json!(0))),
        request(&json!(1), Some(&json!(-1))),
        request(&json!(1), Some(&json!(2_147_483_648_u64))),
    ] {
        assert!(!validator.is_valid(&value));
        assert!(serde_json::from_value::<ReserveNowReference201>(value).is_err());
    }
    let mut unknown_type = request(&json!(1), None);
    unknown_type["connectorType"] = json!("cType9");
    assert!(!validator.is_valid(&unknown_type));
    assert!(serde_json::from_value::<ReserveNowReference201>(unknown_type).is_err());
    for field in ["idToken", "groupIdToken", "customData", "extra"] {
        let mut value = request(&json!(0), Some(&json!(1)));
        value[field] = json!({"idToken":"PRIVATE-RESERVATION","type":"ISO14443"});
        assert!(!validator.is_valid(&value));
        assert!(serde_json::from_value::<ReserveNowReference201>(value).is_err());
    }
    let mut bad_reference = request(&json!(1), None);
    bad_reference["reservationReference"] = json!(format!("reserve16:{}", "a".repeat(64)));
    assert!(serde_json::from_value::<ReserveNowReference201>(bad_reference).is_err());
    let debug = format!(
        "{:?}",
        serde_json::from_value::<ReserveNowReference201>(request(&json!(1), None)).unwrap()
    );
    assert!(!debug.contains(&"a".repeat(64)));
}

#[test]
fn old_results_remain_readable_and_every_native_status_survives_nested_exports() {
    let old: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let mut result: CommandResult = serde_json::from_value(old[1].clone()).unwrap();
    assert!(result.reservation_201.is_none());
    let validator = validator(include_str!("../schemas/v1.12/command-result.schema.json"));
    let export_validator = validator_export();
    for status in [
        ReserveNowStatus201::Accepted,
        ReserveNowStatus201::Faulted,
        ReserveNowStatus201::Occupied,
        ReserveNowStatus201::Rejected,
        ReserveNowStatus201::Unavailable,
    ] {
        for evse_id in [None, Some(1), Some(i32::MAX.cast_unsigned())] {
            let evidence = ReservationResult201::ReserveNow {
                reservation_id: i32::MIN,
                evse_id,
                status: Some(status),
                reconciliation: ReservationReconciliation201 {
                    revision: 7,
                    state: ReservationState201::Removed,
                    observed_at: result.recorded_at,
                    source_time: None,
                },
            };
            result.schema_version = ContractVersion::V1_RESERVATION_201;
            result.lifecycle = CommandLifecycle::ProtocolResponse {
                accepted: evidence.accepted(),
                error: None,
            };
            result.reservation_201 = Some(evidence.clone());
            let encoded = serde_json::to_value(&result).unwrap();
            assert!(validator.is_valid(&encoded));
            assert_eq!(
                serde_json::from_value::<CommandResult>(encoded)
                    .unwrap()
                    .reservation_201,
                Some(evidence)
            );
            let batch: ExportBatch =
                serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).unwrap();
            let record = ExportRecord::new(
                batch.records()[0].metadata().clone(),
                ExportPayload::CommandResult(result.clone()),
            );
            assert_eq!(record.metadata().schema_version.revision, 14);
            assert!(export_validator.is_valid(&serde_json::to_value(record).unwrap()));
        }
    }
    for status in [
        CancelReservationStatus201::Accepted,
        CancelReservationStatus201::Rejected,
    ] {
        let value = json!({"action":"CancelReservation","reservation_id":0,"status":status,"reconciliation":{"revision":8,"state":"cancelled","observed_at":result.recorded_at}});
        let evidence: ReservationResult201 = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            evidence.accepted(),
            status == CancelReservationStatus201::Accepted
        );
        assert!(evidence.has_native_status());
        for field in [
            "idToken",
            "groupIdToken",
            "token_key",
            "group_key",
            "reservationReference",
            "statusInfo",
        ] {
            let mut private = value.clone();
            private[field] = json!("PRIVATE-MARKER");
            assert!(serde_json::from_value::<ReservationResult201>(private).is_err());
        }
    }
    for state in [
        "pending",
        "active",
        "uncertain",
        "rejected",
        "consumed",
        "cancelled",
        "expired",
        "removed",
        "superseded",
        "ambiguous",
    ] {
        let value = json!({"action":"ReserveNow","reservation_id":-1,"reconciliation":{"revision":1,"state":state,"observed_at":result.recorded_at}});
        let evidence: ReservationResult201 = serde_json::from_value(value).unwrap();
        assert!(!evidence.has_native_status());
        assert!(!evidence.accepted());
    }
    assert!(
        serde_json::from_value::<ReservationReconciliation201>(
            json!({"revision":1,"state":"faulted","observed_at":result.recorded_at})
        )
        .is_err(),
        "2.0.1 has no inferred Faulted reservation state"
    );
}

fn validator_export() -> jsonschema::Validator {
    validator(include_str!("../schemas/v1.13/export-record.schema.json"))
}
