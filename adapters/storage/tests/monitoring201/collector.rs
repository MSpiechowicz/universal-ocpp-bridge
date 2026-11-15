//! The collector may finish a report before the application records the native acknowledgement.
use super::support::*;
use std::time::Duration;
use uob_application::{DeviceModelStore201, MonitoringReportStore201};
use uob_contracts::*;

#[tokio::test]
async fn a_report_finished_before_the_application_write_survives_it() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "early", "GetMonitoringReport").await;
    let pending = report_evidence(MonitoringReportState201::Pending);

    let finished = store
        .finish_monitoring_report(
            command.request_id.clone(),
            report_evidence(complete(2)),
            ACCEPTED,
            command.admitted_at,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished.monitoring_201, Some(report_evidence(complete(2))));
    assert_eq!(finished.lifecycle, ACCEPTED);
    assert_eq!(finished.schema_version, ContractVersion::V1_MONITORING_201);
    assert_eq!(database.report_pending("early"), 0);

    // The application's later write of the pending acknowledgement adopts the finished report
    // and cannot move the lifecycle backwards.
    write(&store, monitoring(&command, pending)).await.unwrap();
    let stored = read(&store, "early").await;
    assert_eq!(stored.monitoring_201, Some(report_evidence(complete(2))));
    assert_eq!(stored.lifecycle, ACCEPTED);
    assert_eq!(database.report_pending("early"), 0);

    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    reopened.interrupt_device_reports().await.unwrap();
    assert_eq!(read(&reopened, "early").await, stored);
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn finishing_twice_answers_the_stored_terminal_result_and_changes_nothing() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "twice", "GetMonitoringReport").await;
    let first = store
        .finish_monitoring_report(
            command.request_id.clone(),
            report_evidence(complete(1)),
            ACCEPTED,
            command.admitted_at,
        )
        .await
        .unwrap()
        .unwrap();

    let failed = report_evidence(MonitoringReportState201::Incomplete {
        reason: DeviceReportFailure201::StorageUnavailable,
        progress: None,
    });
    let second = store
        .finish_monitoring_report(
            command.request_id.clone(),
            failed,
            ACCEPTED,
            at("2030-01-01T00:00:00Z"),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(second, first);
    assert_eq!(read(&store, "twice").await, first);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_finished_report_for_another_request_echo_or_acknowledgement_is_an_integrity_failure() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "other", "GetMonitoringReport").await;
    write(
        &store,
        monitoring(&command, report_evidence(MonitoringReportState201::Pending)),
    )
    .await
    .unwrap();

    let mut other_query = report_evidence(complete(1));
    other_query.query = MonitoringQuery201::GetMonitoringReport {
        request_id: 9,
        criteria: vec![],
        component_variables: vec![],
    };
    let mut other_ack = report_evidence(complete(1));
    other_ack.outcome = MonitoringOutcome201::Report {
        ack: Some(DeviceReportAck201::EmptyResultSet),
        report: complete(1),
    };
    for evidence in [other_query, other_ack] {
        assert_eq!(
            store
                .finish_monitoring_report(
                    command.request_id.clone(),
                    evidence,
                    ACCEPTED,
                    command.admitted_at,
                )
                .await
                .unwrap_err()
                .code(),
            uob_application::StorageErrorCode::IntegrityFailure
        );
    }
    assert_eq!(
        read(&store, "other").await.monitoring_201,
        Some(report_evidence(MonitoringReportState201::Pending))
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_late_collector_cannot_complete_a_report_the_restart_interrupted() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "late", "GetMonitoringReport").await;
    write(
        &store,
        monitoring(&command, report_evidence(MonitoringReportState201::Pending)),
    )
    .await
    .unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();

    let reopened = database.open();
    reopened.interrupt_device_reports().await.unwrap();
    let interrupted = report_evidence(MonitoringReportState201::Incomplete {
        reason: DeviceReportFailure201::Interrupted,
        progress: None,
    });
    let late = reopened
        .finish_monitoring_report(
            command.request_id,
            report_evidence(complete(1)),
            ACCEPTED,
            command.admitted_at,
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(late.monitoring_201, Some(interrupted));
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
