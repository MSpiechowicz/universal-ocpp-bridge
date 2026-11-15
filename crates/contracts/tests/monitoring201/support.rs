use serde_json::{Value, json};
use uob_contracts::*;

pub fn validator(source: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(source).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
}

pub fn station() -> StationId {
    StationId::new("station-126").unwrap()
}

pub fn timestamp(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}

pub fn component() -> DeviceComponent201 {
    DeviceComponent201 {
        name: "EVSE".to_owned(),
        instance: None,
        evse: Some(DeviceEvse201 {
            id: 1,
            connector_id: Some(2),
        }),
    }
}

pub fn variable() -> DeviceVariable201 {
    DeviceVariable201 {
        name: "Power".to_owned(),
        instance: Some("Import".to_owned()),
    }
}

pub fn severity(value: u8) -> MonitorSeverity201 {
    MonitorSeverity201::new(value).unwrap()
}

pub fn set_monitor(id: Option<i32>) -> SetMonitor201 {
    SetMonitor201 {
        id,
        transaction: Some(true),
        value: "7400.125".parse().unwrap(),
        monitor_type: MonitorType201::UpperThreshold,
        severity: severity(3),
        component: component(),
        variable: variable(),
    }
}

pub fn reported() -> ReportedMonitor201 {
    ReportedMonitor201 {
        component: component(),
        variable: variable(),
        id: i32::MIN,
        transaction: false,
        value: "-0.5".parse().unwrap(),
        monitor_type: MonitorType201::Delta,
        severity: severity(9),
    }
}

pub fn results() -> Vec<MonitoringResult201> {
    [set_and_clear(), reports(), base_and_level()].concat()
}

fn set_and_clear() -> Vec<MonitoringResult201> {
    let set_result = |status, id| SetMonitorResult201 {
        id,
        status,
        reason_code: (status != SetMonitoringStatus201::Accepted)
            .then(|| "NotSupported".to_owned()),
        monitor_type: MonitorType201::UpperThreshold,
        severity: severity(3),
        component: component(),
        variable: variable(),
    };
    vec![
        MonitoringResult201 {
            query: MonitoringQuery201::SetVariableMonitoring {
                monitors: vec![set_monitor(None), set_monitor(Some(7))],
            },
            outcome: MonitoringOutcome201::SetMonitors {
                monitors: vec![
                    set_result(SetMonitoringStatus201::Accepted, Some(7)),
                    set_result(SetMonitoringStatus201::Duplicate, None),
                ],
            },
        },
        MonitoringResult201 {
            query: MonitoringQuery201::ClearVariableMonitoring { ids: vec![7, 8] },
            outcome: MonitoringOutcome201::ClearMonitors {
                monitors: vec![
                    ClearMonitorResult201 {
                        id: 7,
                        status: ClearMonitoringStatus201::Accepted,
                        reason_code: None,
                    },
                    ClearMonitorResult201 {
                        id: 8,
                        status: ClearMonitoringStatus201::NotFound,
                        reason_code: Some("NotFound".to_owned()),
                    },
                ],
            },
        },
    ]
}

fn reports() -> Vec<MonitoringResult201> {
    let complete = MonitoringReportState201::Complete {
        fragments: vec![MonitoringReportFragment201 {
            generated_at: timestamp("2026-10-08T10:00:00Z"),
            sequence: 0,
            more: false,
            monitors: 2,
        }],
        monitors: vec![reported()],
        monitors_omitted: 1,
    };
    vec![
        MonitoringResult201 {
            query: MonitoringQuery201::GetMonitoringReport {
                request_id: i32::MIN,
                criteria: vec![MonitoringCriterion201::ThresholdMonitoring],
                component_variables: vec![
                    DeviceSelector201 {
                        component: component(),
                        variable: Some(variable()),
                    },
                    DeviceSelector201 {
                        component: component(),
                        variable: None,
                    },
                ],
            },
            outcome: MonitoringOutcome201::Report {
                ack: Some(DeviceReportAck201::Accepted),
                report: complete,
            },
        },
        MonitoringResult201 {
            query: MonitoringQuery201::GetMonitoringReport {
                request_id: 4,
                criteria: Vec::new(),
                component_variables: Vec::new(),
            },
            outcome: MonitoringOutcome201::Report {
                ack: Some(DeviceReportAck201::Accepted),
                report: MonitoringReportState201::Incomplete {
                    reason: DeviceReportFailure201::Timeout,
                    progress: Some(DeviceReportProgress201 {
                        fragments: 1,
                        items: 3,
                        bytes: 900,
                    }),
                },
            },
        },
    ]
}

fn base_and_level() -> Vec<MonitoringResult201> {
    vec![
        MonitoringResult201 {
            query: MonitoringQuery201::SetMonitoringBase {
                base: MonitoringBase201::HardWiredOnly,
            },
            outcome: MonitoringOutcome201::Base {
                status: DeviceReportAck201::EmptyResultSet,
            },
        },
        MonitoringResult201 {
            query: MonitoringQuery201::SetMonitoringLevel {
                severity: severity(0),
            },
            outcome: MonitoringOutcome201::Level {
                status: MonitoringLevelStatus201::Rejected,
            },
        },
    ]
}
