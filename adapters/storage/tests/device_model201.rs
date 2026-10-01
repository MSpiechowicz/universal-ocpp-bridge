use serde_json::{Value, json};
use std::time::Duration;
use uob_application::{AtomicStoreWrite, DeviceModelStore201, OperationalStore};
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
#[path = "device_model201/fixtures.rs"]
mod fixtures;
use fixtures::*;

#[tokio::test]
async fn staged_report_is_not_public_until_ack_and_negative_ack_discards_payload_with_known_counts()
{
    for accepted in [true, false] {
        let database = Database::new();
        let store = database.open();
        let (command, initial) = fixture("staging", None);
        admit(&store, command.clone(), initial.clone()).await;
        let full = complete(initial.device_model_201.as_ref().unwrap());
        let staged = store
            .finish_device_report(command.request_id.clone(), full.clone(), None, at(3))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            staged.device_model_201.unwrap().report,
            DeviceReportState201::Pending
        );
        let mut ack = initial.device_model_201.clone().unwrap();
        ack.native_ack = Some(if accepted {
            DeviceReportAck201::Accepted
        } else {
            DeviceReportAck201::Rejected
        });
        let result = store
            .finish_device_report(
                command.request_id.clone(),
                ack,
                Some(CommandLifecycle::ProtocolResponse {
                    accepted,
                    error: None,
                }),
                at(4),
            )
            .await
            .unwrap()
            .unwrap();
        match result.device_model_201.unwrap().report {
            DeviceReportState201::Complete {
                progress, items, ..
            } if accepted => {
                assert_eq!(progress.items, 1);
                assert_eq!(items[0].variable.name, "PrivateKey");
            }
            DeviceReportState201::Incomplete {
                reason: DeviceReportFailure201::NativeRejected,
                progress: Some(progress),
            } if !accepted => assert_eq!(progress.items, 1),
            other => panic!("unexpected merge: {other:?}"),
        }
        assert_eq!(database.staging_count(), 0);
        assert_eq!(database.pending_count(), 0);
        let repeated = store
            .finish_device_report(command.request_id, full, None, at(5))
            .await
            .unwrap()
            .unwrap();
        assert!(!repeated.device_model_201.unwrap().report.pending());
        store.shutdown(Duration::from_secs(1)).await.unwrap();
    }
}

#[tokio::test]
async fn startup_interrupts_terminal_native_pending_reports_but_opening_a_reader_does_not() {
    let database = Database::new();
    let store = database.open();
    let (command, initial) = fixture("restart", Some(DeviceReportAck201::Accepted));
    admit(&store, command.clone(), initial).await;
    let reader = database.open();
    assert_eq!(
        reader
            .device_model_result(command.request_id.clone())
            .await
            .unwrap()
            .unwrap()
            .device_model_201
            .unwrap()
            .report,
        DeviceReportState201::Pending
    );
    reader.shutdown(Duration::from_secs(1)).await.unwrap();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let recovered = database.open();
    recovered.interrupt_device_reports().await.unwrap();
    let result = recovered
        .device_model_result(command.request_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
    assert_eq!(
        result.device_model_201.unwrap().report,
        DeviceReportState201::Incomplete {
            reason: DeviceReportFailure201::Interrupted,
            progress: None
        }
    );
    assert_eq!(database.pending_count(), 0);
    recovered.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn immutable_generation_conflict_rolls_back_and_stale_ack_cannot_restore_interrupted_inventory()
 {
    let database = Database::new();
    let store = database.open();
    let (command, initial) = fixture("generation", None);
    admit(&store, command.clone(), initial.clone()).await;
    let mut wrong = initial.device_model_201.clone().unwrap();
    wrong.generation += 1;
    assert!(
        store
            .finish_device_report(command.request_id.clone(), wrong, None, at(3))
            .await
            .is_err()
    );
    store.interrupt_device_reports().await.unwrap();
    let mut stale = complete(initial.device_model_201.as_ref().unwrap());
    stale.native_ack = Some(DeviceReportAck201::Accepted);
    let result = store
        .finish_device_report(
            command.request_id,
            stale,
            Some(CommandLifecycle::ProtocolResponse {
                accepted: true,
                error: None,
            }),
            at(4),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result.device_model_201.unwrap().report,
        DeviceReportState201::Incomplete {
            reason: DeviceReportFailure201::Interrupted,
            progress: None
        }
    );
    assert_eq!(database.staging_count(), 0);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn escaped_canonical_output_is_bounded_without_truncating_a_successful_inventory() {
    let database = Database::new();
    let store = database.open();
    let (command, initial) = fixture("output-limit", Some(DeviceReportAck201::Accepted));
    admit(&store, command.clone(), initial.clone()).await;
    let mut full = complete(initial.device_model_201.as_ref().unwrap());
    let DeviceReportState201::Complete { items, .. } = &mut full.report else {
        unreachable!()
    };
    items[0].variable.name = "\u{0001}".repeat(DEVICE_MODEL_OUTPUT_LIMIT_201 / 6);
    let result = store
        .finish_device_report(command.request_id, full, None, at(4))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.device_model_201.as_ref().unwrap().report,
        DeviceReportState201::Incomplete {
            reason: DeviceReportFailure201::OutputLimit,
            ..
        }
    ));
    assert!(serde_json::to_vec(&result).unwrap().len() <= DEVICE_MODEL_OUTPUT_LIMIT_201);
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
