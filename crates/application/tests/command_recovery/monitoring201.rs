//! Finalization of native OCPP 2.0.1 monitoring replies into durable command results.
use super::*;
use uob_contracts::{
    ClearMonitorResult201, ClearMonitoringStatus201, DeviceComponent201, DeviceReportAck201,
    DeviceVariable201, MonitorSeverity201, MonitorType201, MonitoringBase201,
    MonitoringLevelStatus201, MonitoringOutcome201, MonitoringQuery201, MonitoringReportState201,
    MonitoringResult201, SetMonitorResult201, SetMonitoringStatus201,
};

fn set(status: SetMonitoringStatus201) -> SetMonitorResult201 {
    SetMonitorResult201 {
        id: status.accepted().then_some(7),
        status,
        reason_code: None,
        monitor_type: MonitorType201::UpperThreshold,
        severity: MonitorSeverity201::new(3).expect("severity"),
        component: DeviceComponent201 {
            name: "EVSE".to_owned(),
            instance: None,
            evse: None,
        },
        variable: DeviceVariable201 {
            name: "Power".to_owned(),
            instance: None,
        },
    }
}

fn clear(status: ClearMonitoringStatus201) -> ClearMonitorResult201 {
    ClearMonitorResult201 {
        id: 7,
        status,
        reason_code: None,
    }
}

fn evidence(outcome: MonitoringOutcome201) -> MonitoringResult201 {
    let query = match &outcome {
        MonitoringOutcome201::SetMonitors { .. } => MonitoringQuery201::SetVariableMonitoring {
            monitors: Vec::new(),
        },
        MonitoringOutcome201::ClearMonitors { .. } => {
            MonitoringQuery201::ClearVariableMonitoring { ids: vec![7] }
        }
        MonitoringOutcome201::Base { .. } => MonitoringQuery201::SetMonitoringBase {
            base: MonitoringBase201::All,
        },
        MonitoringOutcome201::Level { .. } => MonitoringQuery201::SetMonitoringLevel {
            severity: MonitorSeverity201::new(4).expect("severity"),
        },
        MonitoringOutcome201::Report { .. } => MonitoringQuery201::GetMonitoringReport {
            request_id: 9,
            criteria: Vec::new(),
            component_variables: Vec::new(),
        },
    };
    MonitoringResult201 { query, outcome }
}

fn finalized(evidence: &MonitoringResult201) -> CommandResult {
    block_on(async {
        let stations = Arc::new(Stations::new(
            connected(),
            CommandDispatchOutcome::MonitoringResponse201(Box::new(evidence.clone())),
        ));
        let result = coordinator(Arc::new(MemoryStore::default()), stations)
            .submit(external())
            .await
            .expect("monitoring result");
        assert_eq!(result.monitoring_201.as_ref(), Some(evidence));
        assert_eq!(result.schema_version, ContractVersion::V1_MONITORING_201);
        assert_eq!(result.schema_version.revision, 18);
        result
    })
}

fn rejected() -> CommandLifecycle {
    CommandLifecycle::ProtocolResponse {
        accepted: false,
        error: Some(CommandError {
            code: CommandErrorCode::ProtocolRejected,
            detail: None,
        }),
    }
}

fn accepted() -> CommandLifecycle {
    CommandLifecycle::ProtocolResponse {
        accepted: true,
        error: None,
    }
}

#[test]
fn a_set_or_clear_is_accepted_only_when_every_item_is() {
    let all = |statuses: &[SetMonitoringStatus201]| {
        evidence(MonitoringOutcome201::SetMonitors {
            monitors: statuses.iter().map(|status| set(*status)).collect(),
        })
    };

    assert_eq!(
        finalized(&all(&[SetMonitoringStatus201::Accepted; 2])).lifecycle,
        accepted()
    );
    assert_eq!(
        finalized(&all(&[
            SetMonitoringStatus201::Accepted,
            SetMonitoringStatus201::Duplicate,
        ]))
        .lifecycle,
        rejected()
    );

    let clear = |status| {
        evidence(MonitoringOutcome201::ClearMonitors {
            monitors: vec![clear(status)],
        })
    };
    assert_eq!(
        finalized(&clear(ClearMonitoringStatus201::Accepted)).lifecycle,
        accepted()
    );
    assert_eq!(
        finalized(&clear(ClearMonitoringStatus201::NotFound)).lifecycle,
        rejected()
    );
}

#[test]
fn base_and_level_follow_the_native_status() {
    let base = |status| evidence(MonitoringOutcome201::Base { status });
    assert_eq!(
        finalized(&base(DeviceReportAck201::Accepted)).lifecycle,
        accepted()
    );
    assert_eq!(
        finalized(&base(DeviceReportAck201::Rejected)).lifecycle,
        rejected()
    );

    let level = |status| evidence(MonitoringOutcome201::Level { status });
    assert_eq!(
        finalized(&level(MonitoringLevelStatus201::Accepted)).lifecycle,
        accepted()
    );
    assert_eq!(
        finalized(&level(MonitoringLevelStatus201::Rejected)).lifecycle,
        rejected()
    );
}

#[test]
fn an_accepted_report_is_recorded_pending_until_collection_finishes() {
    let report = |ack, report| evidence(MonitoringOutcome201::Report { ack, report });

    let pending = report(
        Some(DeviceReportAck201::Accepted),
        MonitoringReportState201::Pending,
    );
    assert!(pending.pending());
    assert_eq!(finalized(&pending).lifecycle, accepted());

    let not_expected = report(
        Some(DeviceReportAck201::EmptyResultSet),
        MonitoringReportState201::NotExpected,
    );
    assert_eq!(finalized(&not_expected).lifecycle, rejected());
}
