#[path = "monitoring201/collector.rs"]
mod collector;
#[path = "monitoring201/events.rs"]
mod events;
#[path = "monitoring201/support.rs"]
mod support;

use std::time::Duration;
use support::*;
use uob_application::{
    ChargingProfileReportStore201, DeviceModelStore201, MonitoringReportStore201, OperationalStore,
    StorageErrorCode,
};
use uob_contracts::*;

#[tokio::test]
async fn native_replies_persist_unchanged_at_the_monitoring_revision() {
    let database = Database::new();
    let store = database.open();
    let cases = [
        ("set", "SetVariableMonitoring", set_evidence()),
        ("clear", "ClearVariableMonitoring", clear_evidence()),
        ("base", "SetMonitoringBase", base_evidence()),
        ("level", "SetMonitoringLevel", level_evidence()),
        (
            "rejected-report",
            "GetMonitoringReport",
            MonitoringResult201 {
                query: report_evidence(MonitoringReportState201::NotExpected).query,
                outcome: MonitoringOutcome201::Report {
                    ack: Some(DeviceReportAck201::Rejected),
                    report: MonitoringReportState201::NotExpected,
                },
            },
        ),
    ];

    for (id, action, evidence) in cases {
        let command = admit(&store, id, action).await;
        write(&store, monitoring(&command, evidence.clone()))
            .await
            .unwrap();

        let stored = read(&store, id).await;
        assert_eq!(stored.monitoring_201, Some(evidence));
        assert_eq!(stored.schema_version, ContractVersion::V1_MONITORING_201);
        assert_eq!(database.report_pending(id), 0);
    }

    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(
        read(&reopened, "clear").await.monitoring_201,
        Some(clear_evidence())
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn pending_report_is_finished_once_and_stays_terminal() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "report", "GetMonitoringReport").await;
    let request = command.request_id.clone();
    let now = command.admitted_at;
    let pending = report_evidence(MonitoringReportState201::Pending);

    // An unknown request is distinct from a known one: nothing is recorded for it.
    assert_eq!(
        store
            .finish_monitoring_report(
                RequestId::new("unknown").unwrap(),
                report_evidence(complete(1)),
                ACCEPTED,
                now
            )
            .await
            .unwrap(),
        None
    );

    write(&store, monitoring(&command, pending.clone()))
        .await
        .unwrap();
    assert_eq!(database.report_pending("report"), 1);
    assert_eq!(
        read(&store, "report").await.monitoring_201,
        Some(pending.clone())
    );
    // Collection is never finished with a report that is still pending.
    assert!(
        store
            .finish_monitoring_report(
                request.clone(),
                report_evidence(MonitoringReportState201::Pending),
                ACCEPTED,
                now
            )
            .await
            .is_err()
    );

    let finished = store
        .finish_monitoring_report(request.clone(), report_evidence(complete(2)), ACCEPTED, now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished.monitoring_201, Some(report_evidence(complete(2))));
    assert_eq!(finished.lifecycle, ACCEPTED);
    assert_eq!(database.report_pending("report"), 0);
    assert_eq!(read(&store, "report").await, finished);

    // A second finish is answered with the stored terminal result, and the coordinator's stale
    // copy of the acknowledgement cannot reopen or replace the terminal report.
    let failed = report_evidence(MonitoringReportState201::Incomplete {
        reason: DeviceReportFailure201::Timeout,
        progress: None,
    });
    assert_eq!(
        store
            .finish_monitoring_report(request, failed, ACCEPTED, now)
            .await
            .unwrap(),
        Some(finished.clone())
    );
    write(&store, monitoring(&command, pending)).await.unwrap();
    assert_eq!(read(&store, "report").await, finished);
    assert_eq!(database.report_pending("report"), 0);

    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    assert_eq!(read(&reopened, "report").await, finished);
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_complete_report_over_the_output_limit_becomes_an_explicit_failure() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "large", "GetMonitoringReport").await;
    write(
        &store,
        monitoring(&command, report_evidence(MonitoringReportState201::Pending)),
    )
    .await
    .unwrap();

    let stored = store
        .finish_monitoring_report(
            command.request_id.clone(),
            report_evidence(complete(6_000)),
            ACCEPTED,
            command.admitted_at,
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        stored.monitoring_201,
        Some(report_evidence(MonitoringReportState201::Incomplete {
            reason: DeviceReportFailure201::OutputLimit,
            progress: None,
        }))
    );
    assert_eq!(database.report_pending("large"), 0);
    assert!(serde_json::to_vec(&stored).unwrap().len() < MONITORING_REPORT_OUTPUT_LIMIT_201);

    // A row that somehow holds an over-limit report is corrupt, never served.
    let oversized = monitoring(&command, report_evidence(complete(6_000)));
    database
        .connection()
        .execute(
            "UPDATE command_results SET payload = ?2 WHERE request_id = ?1",
            ("large", serde_json::to_string(&oversized).unwrap()),
        )
        .unwrap();
    assert!(
        store
            .command_result_by_request_id(command.request_id)
            .await
            .is_err()
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn startup_interrupts_pending_monitoring_reports_and_keeps_other_families_as_before() {
    let database = Database::new();
    let store = database.open();

    let command = admit(&store, "monitoring", "GetMonitoringReport").await;
    write(
        &store,
        monitoring(&command, report_evidence(MonitoringReportState201::Pending)),
    )
    .await
    .unwrap();

    let profiles = admit(&store, "profiles", "GetChargingProfiles").await;
    store
        .finish_charging_profiles(
            profiles.request_id.clone(),
            ChargingProfilesResult201 {
                query: ChargingProfilesQuery201 {
                    request_id: 4,
                    evse_id: None,
                    charging_profile_purpose: None,
                    stack_level: Some(0),
                    charging_profile_id: vec![],
                    charging_limit_source: vec![],
                },
                status: ChargingProfilesStatus201::Accepted,
                reason_code: None,
                report: ChargingProfileReportState201::Pending,
            },
            Some(ACCEPTED),
            profiles.admitted_at,
        )
        .await
        .unwrap();

    let device = admit(&store, "device", "GetBaseReport").await;
    let mut pending_device = result(&device, ACCEPTED);
    pending_device.schema_version = ContractVersion::V1_DEVICE_MODEL_201;
    pending_device.device_model_201 = Some(DeviceModelResult201 {
        query: DeviceModelQuery201::GetBaseReport {
            request_id: 8,
            report_base: DeviceReportBase201::FullInventory,
        },
        connection: CorrelationId::new("connection").unwrap(),
        generation: 1,
        dispatch_recorded_at: device.admitted_at,
        native_ack: Some(DeviceReportAck201::Accepted),
        variables: vec![],
        report: DeviceReportState201::Pending,
    });
    write(&store, pending_device).await.unwrap();

    let finished = admit(&store, "finished", "GetMonitoringReport").await;
    write(&store, monitoring(&finished, report_evidence(complete(1))))
        .await
        .unwrap();

    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    reopened.interrupt_device_reports().await.unwrap();

    let interrupted = read(&reopened, "monitoring").await;
    assert_eq!(interrupted.lifecycle, ACCEPTED);
    assert_eq!(
        interrupted.monitoring_201,
        Some(report_evidence(MonitoringReportState201::Incomplete {
            reason: DeviceReportFailure201::Interrupted,
            progress: None,
        }))
    );
    assert_eq!(database.report_pending("monitoring"), 0);
    assert_eq!(
        read(&reopened, "profiles")
            .await
            .charging_profiles_201
            .unwrap()
            .report,
        ChargingProfileReportState201::Incomplete {
            reason: ChargingProfileReportFailure201::Interrupted,
            progress: None,
        }
    );
    assert_eq!(
        read(&reopened, "device")
            .await
            .device_model_201
            .unwrap()
            .report,
        DeviceReportState201::Incomplete {
            reason: DeviceReportFailure201::Interrupted,
            progress: None,
        }
    );
    // A completed report is untouched.
    assert_eq!(
        read(&reopened, "finished").await.monitoring_201,
        Some(report_evidence(complete(1)))
    );
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn inconsistent_evidence_is_refused_and_leaves_no_trace() {
    let database = Database::new();
    let store = database.open();
    let command = admit(&store, "invalid", "SetVariableMonitoring").await;
    let invalid = StorageErrorCode::InvalidRequest;

    // Query and reply must be the same native action.
    let mut mismatched = set_evidence();
    mismatched.outcome = base_evidence().outcome;
    assert_eq!(
        write(&store, monitoring(&command, mismatched))
            .await
            .unwrap_err()
            .code(),
        invalid
    );

    // One independent result per requested monitor, and per requested ID.
    let mut fewer = set_evidence();
    fewer.outcome = MonitoringOutcome201::SetMonitors { monitors: vec![] };
    assert_eq!(
        write(&store, monitoring(&command, fewer))
            .await
            .unwrap_err()
            .code(),
        invalid
    );
    let mut other_ids = clear_evidence();
    other_ids.outcome = MonitoringOutcome201::ClearMonitors {
        monitors: vec![clear_result(4), clear_result(6)],
    };
    assert_eq!(
        write(&store, monitoring(&command, other_ids))
            .await
            .unwrap_err()
            .code(),
        invalid
    );

    // Only the monitoring revision may carry monitoring evidence.
    for schema_version in [
        ContractVersion::V1_INITIAL,
        ContractVersion::V1_DIAGNOSTICS_201,
    ] {
        let mut stale = monitoring(&command, set_evidence());
        stale.schema_version = schema_version;
        assert_eq!(write(&store, stale).await.unwrap_err().code(), invalid);
    }

    // No other feature's evidence may ride along, whichever revision is claimed.
    for schema_version in [
        ContractVersion::V1_MONITORING_201,
        ContractVersion::V1_CONFIGURATION_201,
    ] {
        let mut mixed = monitoring(&command, set_evidence());
        mixed.schema_version = schema_version;
        mixed.configuration_201 = Some(ConfigurationResult201::SetNetworkProfile {
            configuration_slot: 0,
            status: SetNetworkProfileStatus201::Accepted,
            staged: true,
        });
        assert!(write(&store, mixed).await.is_err());
    }

    // The failed writes changed nothing.
    assert_eq!(read(&store, "invalid").await.monitoring_201, None);
    assert_eq!(
        read(&store, "invalid").await.lifecycle,
        CommandLifecycle::Dispatched
    );

    // A recorded native reply is an identity, never rewritten by another one.
    write(&store, monitoring(&command, set_evidence()))
        .await
        .unwrap();
    let mut conflicting = set_evidence();
    conflicting.outcome = MonitoringOutcome201::SetMonitors {
        monitors: vec![set_result(SetMonitoringStatus201::Rejected)],
    };
    assert_eq!(
        write(&store, monitoring(&command, conflicting))
            .await
            .unwrap_err()
            .code(),
        StorageErrorCode::IntegrityFailure
    );
    assert_eq!(
        read(&store, "invalid").await.monitoring_201,
        Some(set_evidence())
    );
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn schema_v21_database_opens_as_v22_and_a_newer_one_is_refused() {
    let database = Database::new();
    drop(database.open());
    database
        .connection()
        .execute_batch("PRAGMA user_version = 21;")
        .unwrap();

    let store = database.open();
    let command = admit(&store, "after-upgrade", "GetMonitoringReport").await;
    write(
        &store,
        monitoring(&command, report_evidence(MonitoringReportState201::Pending)),
    )
    .await
    .unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    drop(store);
    assert_eq!(database.version(), 22);

    database
        .connection()
        .execute_batch("PRAGMA user_version = 23;")
        .unwrap();
    assert!(Store::open(&database.0, 16).is_err());
    assert_eq!(database.version(), 23);
}
