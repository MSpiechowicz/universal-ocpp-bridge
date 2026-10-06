use serde_json::{Value, json};
use uob_contracts::*;

fn validator(path: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(path).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
}

fn base() -> CommandResult {
    let old: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let result: CommandResult = serde_json::from_value(old[1].clone()).unwrap();
    assert!(result.composite_schedule_201.is_none() && result.charging_profiles_201.is_none());
    result
}

fn period(limit: &str) -> ChargingSchedulePeriod201 {
    ChargingSchedulePeriod201 {
        start_period: 0,
        limit: serde_json::from_value(json!(limit)).unwrap(),
        number_phases: Some(1),
        phase_to_use: Some(3),
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Keep both evidence shapes beside their schema checks.
fn composite_and_profile_report_evidence_validate_in_results_and_nested_exports() {
    let result_validator = validator(include_str!("../schemas/v1.13/command-result.schema.json"));
    let export_validator = validator(include_str!("../schemas/v1.14/export-record.schema.json"));
    let mut result = base();
    result.schema_version = ContractVersion::V1_SCHEDULES_201;
    result.lifecycle = CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    };
    let schedule = CompositeScheduleResult201 {
        request: CompositeScheduleRequest201 {
            evse_id: 0,
            duration: i32::MAX,
            charging_rate_unit: Some(ChargingScheduleRateUnit201::W),
        },
        status: CompositeScheduleStatus201::Accepted,
        reason_code: Some(SmartChargingReason201::UnsupportedRateUnit),
        schedule: Some(CompositeSchedule201 {
            evse_id: 0,
            duration: i32::MAX,
            schedule_start: result.recorded_at,
            charging_rate_unit: ChargingScheduleRateUnit201::W,
            charging_schedule_period: vec![period("900719925474099.1")],
        }),
    };
    let report = ChargingProfilesResult201 {
        query: ChargingProfilesQuery201 {
            request_id: i32::MIN,
            evse_id: Some(0),
            charging_profile_purpose: Some(
                ReportedChargingProfilePurpose201::ChargingStationExternalConstraints,
            ),
            stack_level: Some(0),
            charging_profile_id: vec![],
            charging_limit_source: vec![ChargingLimitSource201::So, ChargingLimitSource201::Other],
        },
        status: ChargingProfilesStatus201::Accepted,
        reason_code: None,
        report: ChargingProfileReportState201::Complete {
            progress: ChargingProfileReportProgress201 {
                fragments: 1,
                profiles: 1,
                bytes: 512,
            },
            fragments: vec![ChargingProfileReportFragment201 {
                sequence: 0,
                evse_id: 0,
                charging_limit_source: ChargingLimitSource201::So,
                more: false,
                profiles: 1,
            }],
            profiles: vec![ReportedChargingProfile201 {
                evse_id: 0,
                charging_limit_source: ChargingLimitSource201::So,
                id: -1,
                stack_level: 0,
                charging_profile_purpose:
                    ReportedChargingProfilePurpose201::ChargingStationExternalConstraints,
                charging_profile_kind: ChargingProfileKind201::Recurring,
                transaction_id: None,
                recurrency_kind: Some(ChargingProfileRecurrency201::Daily),
                valid_from: Some(result.recorded_at),
                valid_to: None,
                charging_schedule: vec![ChargingSchedule201 {
                    id: 1,
                    duration: Some(86400),
                    start_schedule: Some(result.recorded_at),
                    charging_rate_unit: ChargingScheduleRateUnit201::A,
                    charging_schedule_period: vec![period("0")],
                    min_charging_rate: None,
                }],
                sales_tariff_omitted: true,
            }],
        },
    };
    for (composite, profiles) in [(Some(schedule), None), (None, Some(report))] {
        result.composite_schedule_201 = composite.clone();
        result.charging_profiles_201 = profiles.clone();
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(result_validator.is_valid(&encoded), "{encoded}");
        let decoded: CommandResult = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(
            (
                decoded.composite_schedule_201,
                decoded.charging_profiles_201
            ),
            (composite, profiles)
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
    // Exact rates are strings; a binary number never validates as evidence.
    let mut encoded = serde_json::to_value(&result).unwrap();
    encoded["charging_profiles_201"]["report"]["profiles"][0]["charging_schedule"][0]["charging_schedule_period"]
        [0]["limit"] = json!(0.1);
    assert!(!result_validator.is_valid(&encoded));
    // Older published revisions never describe the additive fields.
    let previous = validator(include_str!("../schemas/v1.12/command-result.schema.json"));
    assert!(previous.is_valid(&serde_json::to_value(base()).unwrap()));
}

#[test]
fn native_reason_codes_match_case_insensitively_and_unlisted_codes_are_dropped() {
    for (native, expected) in [
        ("UnknownEvse", Some(SmartChargingReason201::UnknownEvse)),
        ("unknownevse", Some(SmartChargingReason201::UnknownEvse)),
        ("NOTFOUND", Some(SmartChargingReason201::NotFound)),
        (
            "UnsupportedRateUnit",
            Some(SmartChargingReason201::UnsupportedRateUnit),
        ),
        ("NoError", None),
        ("", None),
    ] {
        assert_eq!(SmartChargingReason201::from_native(native), expected);
    }
    for reason in [
        SmartChargingReason201::DuplicateRequestId,
        SmartChargingReason201::InvalidValue,
        SmartChargingReason201::UnsupportedParam,
        SmartChargingReason201::UnsupportedRequest,
        SmartChargingReason201::InternalError,
    ] {
        assert_eq!(
            SmartChargingReason201::from_native(reason.as_str()),
            Some(reason)
        );
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            json!(reason.as_str())
        );
    }
    assert_eq!(
        serde_json::to_value(ChargingLimitSource201::Ems).unwrap(),
        json!("EMS")
    );
    assert_eq!(
        serde_json::to_value(ChargingProfileReportState201::Pending).unwrap(),
        json!({"state":"pending"})
    );
}
