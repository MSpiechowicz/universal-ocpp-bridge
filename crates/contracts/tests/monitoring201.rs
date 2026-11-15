use serde_json::{Value, json};
use support::*;
use uob_contracts::*;

#[path = "monitoring201/events.rs"]
mod events;
#[path = "monitoring201/support.rs"]
mod support;

#[test]
fn every_result_survives_command_results_and_nested_exports() {
    let old: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    let mut result: CommandResult = serde_json::from_value(old[1].clone()).unwrap();
    assert!(result.monitoring_201.is_none());
    let command_results = validator(include_str!("../schemas/v1.18/command-result.schema.json"));
    let exports = validator(include_str!("../schemas/v1.19/export-record.schema.json"));
    let batch: ExportBatch =
        serde_json::from_str(include_str!("fixtures/export-batch-v1.json")).unwrap();
    assert_eq!(ContractVersion::V1_MONITORING_201.revision, 18);

    for evidence in results() {
        result.schema_version = ContractVersion::V1_MONITORING_201;
        result.monitoring_201 = Some(evidence.clone());
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(command_results.is_valid(&encoded), "{evidence:?}");
        assert_eq!(
            serde_json::from_value::<CommandResult>(encoded)
                .unwrap()
                .monitoring_201,
            Some(evidence)
        );

        let record = ExportRecord::new(
            batch.records()[0].metadata().clone(),
            ExportPayload::CommandResult(result.clone()),
        );
        assert_eq!(record.metadata().schema_version.revision, 18);
        assert!(exports.is_valid(&serde_json::to_value(record).unwrap()));
    }
}

#[test]
fn results_without_monitoring_evidence_keep_their_previous_encoding() {
    let old: Value =
        serde_json::from_str(include_str!("fixtures/command-results-v1.json")).unwrap();
    for stored in old.as_array().unwrap() {
        let decoded: CommandResult = serde_json::from_value(stored.clone()).unwrap();
        assert!(decoded.monitoring_201.is_none());
        assert!(
            serde_json::to_value(&decoded)
                .unwrap()
                .get("monitoring_201")
                .is_none()
        );
    }
}

#[test]
fn accepted_requires_every_native_status_to_be_accepted() {
    let evidence = results();
    let accepted: Vec<bool> = evidence.iter().map(MonitoringResult201::accepted).collect();
    assert_eq!(accepted, [false, false, true, true, false, false]);
    assert!(
        !evidence[2].pending(),
        "a completed report is not pending collection"
    );

    let mut set = evidence[0].clone();
    set.outcome = MonitoringOutcome201::SetMonitors {
        monitors: Vec::new(),
    };
    assert!(!set.accepted(), "an empty reply never counts as accepted");

    let mut report = evidence[2].clone();
    report.outcome = MonitoringOutcome201::Report {
        ack: Some(DeviceReportAck201::Accepted),
        report: MonitoringReportState201::Pending,
    };
    assert!(report.accepted() && report.pending());
    report.outcome = MonitoringOutcome201::Report {
        ack: None,
        report: MonitoringReportState201::NotExpected,
    };
    assert!(!report.accepted() && !report.pending());
    report.outcome = MonitoringOutcome201::Report {
        ack: Some(DeviceReportAck201::NotSupported),
        report: MonitoringReportState201::NotExpected,
    };
    assert!(!report.accepted());

    assert_eq!(evidence[2].query.request_id(), Some(i32::MIN));
    assert_eq!(evidence[0].query.request_id(), None);
}

fn spelled<T: serde::Serialize>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .map(|value| serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap())
        .collect()
}

#[test]
fn native_spellings_match_the_pinned_enumerations() {
    assert_eq!(
        spelled(&[
            MonitorType201::UpperThreshold,
            MonitorType201::LowerThreshold,
            MonitorType201::Delta,
            MonitorType201::Periodic,
            MonitorType201::PeriodicClockAligned,
        ]),
        [
            "UpperThreshold",
            "LowerThreshold",
            "Delta",
            "Periodic",
            "PeriodicClockAligned"
        ]
    );
    assert_eq!(
        spelled(&[
            MonitoringCriterion201::ThresholdMonitoring,
            MonitoringCriterion201::DeltaMonitoring,
            MonitoringCriterion201::PeriodicMonitoring,
        ]),
        [
            "ThresholdMonitoring",
            "DeltaMonitoring",
            "PeriodicMonitoring"
        ]
    );
    assert_eq!(
        spelled(&[
            MonitoringBase201::All,
            MonitoringBase201::FactoryDefault,
            MonitoringBase201::HardWiredOnly,
        ]),
        ["All", "FactoryDefault", "HardWiredOnly"]
    );
}

#[test]
fn native_statuses_and_event_spellings_match_the_pinned_enumerations() {
    assert_eq!(
        spelled(&[
            SetMonitoringStatus201::Accepted,
            SetMonitoringStatus201::UnknownComponent,
            SetMonitoringStatus201::UnknownVariable,
            SetMonitoringStatus201::UnsupportedMonitorType,
            SetMonitoringStatus201::Rejected,
            SetMonitoringStatus201::Duplicate,
        ]),
        [
            "Accepted",
            "UnknownComponent",
            "UnknownVariable",
            "UnsupportedMonitorType",
            "Rejected",
            "Duplicate"
        ]
    );
    assert_eq!(
        spelled(&[
            ClearMonitoringStatus201::Accepted,
            ClearMonitoringStatus201::Rejected,
            ClearMonitoringStatus201::NotFound,
        ]),
        ["Accepted", "Rejected", "NotFound"]
    );
    assert_eq!(
        spelled(&[
            MonitoringLevelStatus201::Accepted,
            MonitoringLevelStatus201::Rejected,
        ]),
        ["Accepted", "Rejected"]
    );
    assert_eq!(
        spelled(&[
            EventTrigger201::Alerting,
            EventTrigger201::Delta,
            EventTrigger201::Periodic,
        ]),
        ["Alerting", "Delta", "Periodic"]
    );
    assert_eq!(
        spelled(&[
            EventNotification201::HardWiredNotification,
            EventNotification201::HardWiredMonitor,
            EventNotification201::PreconfiguredMonitor,
            EventNotification201::CustomMonitor,
        ]),
        [
            "HardWiredNotification",
            "HardWiredMonitor",
            "PreconfiguredMonitor",
            "CustomMonitor"
        ]
    );
    for wrong in ["upperThreshold", "UPPER_THRESHOLD", "Upper", ""] {
        assert!(serde_json::from_value::<MonitorType201>(json!(wrong)).is_err());
    }
    assert!(serde_json::from_value::<SetMonitoringStatus201>(json!("NotSupported")).is_err());
    assert!(serde_json::from_value::<EventTrigger201>(json!("Alert")).is_err());
}

#[test]
fn severity_accepts_only_the_native_range() {
    for valid in 0..=9_u8 {
        let value = MonitorSeverity201::new(valid).unwrap();
        assert_eq!(value.get(), valid);
        assert_eq!(serde_json::to_value(value).unwrap(), json!(valid));
        assert_eq!(
            serde_json::from_value::<MonitorSeverity201>(json!(valid)).unwrap(),
            value
        );
    }
    assert!(MonitorSeverity201::new(10).is_err());
    for invalid in [
        json!(10),
        json!(255),
        json!(256),
        json!(-1),
        json!(1.5),
        json!("5"),
    ] {
        assert!(
            serde_json::from_value::<MonitorSeverity201>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }

    // Every place a severity is carried enforces the range, not just the newtype.
    let mut level = serde_json::to_value(&results()[5].query).unwrap();
    assert_eq!(level["severity"], 0);
    level["severity"] = json!(10);
    assert!(serde_json::from_value::<MonitoringQuery201>(level).is_err());
    let mut monitor = serde_json::to_value(set_monitor(None)).unwrap();
    monitor["severity"] = json!(-3);
    assert!(serde_json::from_value::<SetMonitor201>(monitor).is_err());
    let mut monitor = serde_json::to_value(reported()).unwrap();
    monitor["severity"] = json!(12);
    assert!(serde_json::from_value::<ReportedMonitor201>(monitor).is_err());
}

#[test]
fn monitor_values_are_exact_decimals_and_queries_reject_unknown_input() {
    let monitor = serde_json::to_value(set_monitor(None)).unwrap();
    assert_eq!(monitor["value"], "7400.125");
    assert!(monitor.get("id").is_none(), "an absent ID stays absent");
    assert_eq!(monitor["monitor_type"], "UpperThreshold");
    for value in [json!("1e3"), json!("NaN"), json!(""), json!(" 1")] {
        let mut invalid = monitor.clone();
        invalid["value"] = value.clone();
        assert!(
            serde_json::from_value::<SetMonitor201>(invalid).is_err(),
            "{value}"
        );
    }

    let mut extra = monitor.clone();
    extra["customData"] = json!({"vendorId": "x"});
    assert!(serde_json::from_value::<SetMonitor201>(extra).is_err());
    assert!(
        serde_json::from_value::<MonitoringQuery201>(json!({"action": "GetBaseReport"})).is_err()
    );
    assert!(
        serde_json::from_value::<MonitoringQuery201>(
            json!({"action": "SetMonitoringBase", "base": "Everything"})
        )
        .is_err()
    );
    let query: MonitoringQuery201 =
        serde_json::from_value(json!({"action": "GetMonitoringReport", "request_id": 9})).unwrap();
    assert_eq!(
        query,
        MonitoringQuery201::GetMonitoringReport {
            request_id: 9,
            criteria: Vec::new(),
            component_variables: Vec::new(),
        },
        "omitted criteria and selectors mean none were requested"
    );
}

#[test]
fn reason_codes_are_one_to_twenty_printable_ascii_characters() {
    assert!(valid_monitoring_reason_code_201("NotFound"));
    assert!(valid_monitoring_reason_code_201(&"x".repeat(20)));
    for invalid in ["", "a b", "caf\u{e9}", &"x".repeat(21)] {
        assert!(!valid_monitoring_reason_code_201(invalid), "{invalid:?}");
    }

    let mut set = serde_json::to_value(&results()[0]).unwrap();
    set["outcome"]["monitors"][1]["reason_code"] = json!("x".repeat(21));
    assert!(serde_json::from_value::<MonitoringResult201>(set).is_err());
    let mut clear = serde_json::to_value(&results()[1]).unwrap();
    clear["outcome"]["monitors"][1]["reason_code"] = json!("");
    assert!(serde_json::from_value::<MonitoringResult201>(clear).is_err());
}
