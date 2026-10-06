#[path = "routing_failures_support.rs"]
mod helpers;
#[path = "routing_failures_responsiveness.rs"]
mod responsiveness;

use crate::support::*;
use helpers::*;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;

#[tokio::test]
async fn duplicate_and_skipped_sequences_keep_native_ack_but_discard_partial_inventory() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = station_commands(coordinator, &snapshot);
    for (index, (sequences, reason, count)) in [
        (vec![0, 0], DeviceReportFailure201::DuplicateSequence, 1),
        (vec![1], DeviceReportFailure201::OutOfOrderSequence, 0),
        (vec![0, 2], DeviceReportFailure201::OutOfOrderSequence, 1),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("sequence-{index}");
        let native_id = 10 + i32::try_from(index).unwrap();
        start_report(
            &mut running,
            &commands,
            report_request(
                &snapshot,
                &id,
                "GetBaseReport",
                json!({"requestId":native_id,"reportBase":"FullInventory"}),
                snapshot.station.clone(),
            ),
        )
        .await;
        for (fragment, sequence) in sequences.into_iter().enumerate() {
            notify(
                &mut running,
                notification(
                    &format!("sequence-{index}-fragment-{fragment}"),
                    native_id,
                    sequence,
                    fragment == 0 && count == 1,
                    vec![item(json!({"name":"Ctrlr"}), "Probe")],
                ),
            )
            .await;
        }
        let result = terminal(&store, &id).await;
        incomplete(&result, reason, count, usize::try_from(count).unwrap());
        silence(&mut running).await;
    }
    shutdown(running, store).await;
}

#[tokio::test]
async fn scoped_reports_reject_station_other_evse_and_other_connector_without_expanding_grants() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let before = persisted(&store).await;
    for (index, (resource_index, outside)) in [
        (0, json!({"name":"Ctrlr"})),
        (0, json!({"name":"Ctrlr","evse":{"id":2}})),
        (1, json!({"name":"Ctrlr","evse":{"id":1,"connectorId":2}})),
        (1, json!({"name":"Ctrlr","evse":{"id":1}})),
    ]
    .into_iter()
    .enumerate()
    {
        let resource = snapshot.resources[resource_index].resource.clone();
        let NativeProtocolReference::Ocpp201 {
            evse_id,
            connector_id,
        } = resource.native_protocol_reference.unwrap()
        else {
            panic!("native resource mapping");
        };
        let mut component = json!({"name":"Ctrlr","evse":{"id":evse_id}});
        if let Some(connector_id) = connector_id {
            component["evse"]["connectorId"] = json!(connector_id);
        }
        let commands = Arc::new(ScopedCommandAdmissionPort::new(
            coordinator.clone(),
            AccessPolicy::single(
                AccessGrant::new(
                    origin(),
                    vec![AccessPermission::PrivilegedControl],
                    vec![AccessResourceScope::Resource(resource.clone())],
                )
                .unwrap(),
            ),
        ));
        let id = format!("scope-{index}");
        let native_id = 30 + i32::try_from(index).unwrap();
        let payload = json!({
            "requestId":native_id,
            "componentVariable":[{"component":component,"variable":{"name":"Probe"}}]
        });
        let admitted = start_report(
            &mut running,
            &commands,
            report_request(
                &snapshot,
                &id,
                "GetReport",
                payload.clone(),
                resource.clone(),
            ),
        )
        .await;
        notify(
            &mut running,
            notification(
                &format!("scope-fragment-{index}"),
                native_id,
                0,
                false,
                vec![item(component, "Probe"), item(outside, "Escaped")],
            ),
        )
        .await;
        let result = terminal(&store, &id).await;
        incomplete(&result, DeviceReportFailure201::Correlation, 0, 0);
        assert_eq!(result.resource, resource);
        assert_eq!(
            result.device_model_201.as_ref().unwrap().query,
            admitted.device_model_201.unwrap().query
        );
        assert_eq!(persisted(&store).await, before);

        scope_denials(
            &mut running,
            &commands,
            &snapshot,
            index,
            native_id,
            payload,
            resource,
        )
        .await;
    }
    shutdown(running, store).await;
}

#[tokio::test]
async fn report_actions_share_native_ids_while_pending_and_after_completion_without_renumbering() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = station_commands(coordinator, &snapshot);
    for (index, (first_action, terminal_first)) in [
        ("GetBaseReport", false),
        ("GetBaseReport", true),
        ("GetReport", false),
        ("GetReport", true),
    ]
    .into_iter()
    .enumerate()
    {
        let native_id = -100 - i32::try_from(index).unwrap();
        let first_id = format!("id-first-{index}");
        let second_id = format!("id-second-{index}");
        let payload = |action| {
            if action == "GetBaseReport" {
                json!({"requestId":native_id,"reportBase":"SummaryInventory"})
            } else {
                json!({"requestId":native_id})
            }
        };
        start_report(
            &mut running,
            &commands,
            report_request(
                &snapshot,
                &first_id,
                first_action,
                payload(first_action),
                snapshot.station.clone(),
            ),
        )
        .await;
        if terminal_first {
            complete_empty_report(
                &mut running,
                &store,
                &first_id,
                &format!("id-final-{index}"),
                native_id,
            )
            .await;
        }
        let before = result(&store, &first_id).await;
        let second_action = if first_action == "GetReport" {
            "GetBaseReport"
        } else {
            "GetReport"
        };
        let rejected = commands
            .submit(report_request(
                &snapshot,
                &second_id,
                second_action,
                payload(second_action),
                snapshot.station.clone(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            rejected.lifecycle,
            CommandLifecycle::Rejected { ref error }
                if error.code == CommandErrorCode::PolicyRejected
        ));
        let evidence = rejected.device_model_201.unwrap();
        assert_eq!(evidence.query.request_id(), Some(native_id));
        assert_eq!(evidence.native_ack, None);
        assert!(matches!(
            evidence.report,
            DeviceReportState201::Incomplete {
                reason: DeviceReportFailure201::NotTransmitted,
                ..
            }
        ));
        silence(&mut running).await;
        assert_eq!(result(&store, &first_id).await, before);
        if !terminal_first {
            complete_empty_report(
                &mut running,
                &store,
                &first_id,
                &format!("id-pending-final-{index}"),
                native_id,
            )
            .await;
        }
    }
    shutdown(running, store).await;
}

#[tokio::test]
async fn late_and_unsolicited_receipt_acks_cannot_reopen_or_seed_durable_inventory() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let before = persisted(&store).await;
    let commands = station_commands(coordinator, &snapshot);
    for (index, sequence) in [0, 1].into_iter().enumerate() {
        let id = format!("terminal-{index}");
        let native_id = 50 + i32::try_from(index).unwrap();
        start_report(
            &mut running,
            &commands,
            report_request(
                &snapshot,
                &id,
                "GetReport",
                json!({"requestId":native_id}),
                snapshot.station.clone(),
            ),
        )
        .await;
        notify(
            &mut running,
            notification(
                &format!("terminal-initial-{index}"),
                native_id,
                sequence,
                false,
                vec![item(json!({"name":"Ctrlr"}), "Original")],
            ),
        )
        .await;
        let finalized = terminal(&store, &id).await;
        if sequence == 1 {
            incomplete(&finalized, DeviceReportFailure201::OutOfOrderSequence, 0, 0);
        } else {
            let DeviceReportState201::Complete {
                progress, items, ..
            } = &finalized.device_model_201.as_ref().unwrap().report
            else {
                panic!("original complete inventory");
            };
            assert_eq!((progress.fragments, progress.items), (1, 1));
            assert_eq!(items[0].variable.name, "Original");
        }
        notify(
            &mut running,
            notification(
                &format!("terminal-late-{index}"),
                native_id,
                0,
                false,
                vec![item(json!({"name":"Ctrlr"}), "Late")],
            ),
        )
        .await;
        let fresh_native_id = native_id + 100;
        notify(
            &mut running,
            notification(
                &format!("terminal-unsolicited-{index}"),
                fresh_native_id,
                0,
                false,
                vec![item(json!({"name":"Ctrlr"}), "Unsolicited")],
            ),
        )
        .await;
        fresh_inventory(
            &mut running,
            &commands,
            &snapshot,
            &store,
            index,
            fresh_native_id,
        )
        .await;
        silence(&mut running).await;
        assert_eq!(result(&store, &id).await, finalized);
        assert_eq!(persisted(&store).await, before);
    }
    shutdown(running, store).await;
}

#[tokio::test]
async fn default_off_rejects_all_queries_even_with_capabilities_and_notify_report_is_unsupported() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) =
        Box::pin(setup(&store, running.handle.clone())).await;
    for action in ["GetVariables", "GetReport"] {
        snapshot.capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: action.to_owned(),
            },
            parameters: vec![],
        });
    }
    port.update_committed(snapshot.clone()).unwrap();
    let commands = station_commands(coordinator, &snapshot);
    for (action, payload) in [
        (
            "GetVariables",
            json!({"getVariableData":[{"component":{"name":"Ctrlr"},"variable":{"name":"Probe"}}]}),
        ),
        (
            "GetBaseReport",
            json!({"requestId":-1,"reportBase":"FullInventory"}),
        ),
        ("GetReport", json!({"requestId":-2})),
    ] {
        let rejected = commands
            .submit(report_request(
                &snapshot,
                &format!("off-{action}"),
                action,
                payload,
                snapshot.station.clone(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            rejected.lifecycle,
            CommandLifecycle::Rejected { ref error }
                if error.code == CommandErrorCode::UnsupportedOperation
        ));
        assert!(rejected.device_model_201.is_none());
        silence(&mut running).await;
    }
    running
        .peer
        .send_text(notification("off-notification", -1, 0, false, vec![]).to_string())
        .await
        .unwrap();
    let error = receive_json(&mut running.peer).await;
    assert_eq!(error[0], 4);
    assert_eq!(error[1], "off-notification");
    assert_eq!(error[2], "NotImplemented");
    heartbeat(&mut running, "off-heartbeat").await;
    shutdown(running, store).await;
}

#[tokio::test]
async fn sustained_collection_yields_to_application_station_calls_without_losing_report_order() {
    Box::pin(responsiveness::sustained_collection()).await;
}

#[tokio::test]
async fn valid_report_flood_preserves_station_call_handoff_and_critical_response_budget() {
    Box::pin(responsiveness::valid_flood()).await;
}
