use crate::{
    configuration_support::{configured, entry, variables},
    support::*,
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::{OutboundCall, SessionCallOutcome};

fn persisted_result(command: &Command<Value>, lifecycle: CommandLifecycle) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        correlation_id: command.correlation_id.clone(),
        resource: command.resource.clone(),
        return_route: command.return_route(),
        lifecycle,
        recorded_at: command.admitted_at,
        observed_effects: vec![],
        configuration: None,
        configuration_observations: vec![],
        trigger_observation: None,
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: None,
        charging_profile_16: None,
        charging_profile_201: None,
        configuration_201: None,
        local_authorization_16: None,
        local_authorization_201: None,
        reservation_16: None,
        reservation_201: None,
    }
}

#[tokio::test]
async fn recovery_pages_past_retained_states_without_dispatch_and_preserves_pairs_on_reopen() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let binding = entry("Pending", 1);
    let (snapshot, _, port, _) = Box::pin(configured(
        &store,
        &running,
        vec![(binding.clone(), "RECOVERY_SECRET".into())],
        None,
        Arc::new(Clock),
    ))
    .await;
    let expected = seed_retained_commands(&store, &snapshot, &binding).await;
    assert_first_storage_page(&store, &expected).await;
    let coordinator = Coordinator::new(Arc::new(store.clone()), port.clone(), Arc::new(Clock));
    assert_coordinator_recovery_pairs(&coordinator, &expected).await;
    assert_tail_storage_page(&store, &expected).await;
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let reopened = database.open();
    let restarted = Coordinator::new(Arc::new(reopened.clone()), port, Arc::new(Clock));
    assert_reopened_recovery(&restarted, &expected).await;
    assert_recovery_did_not_send(&mut running).await;
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

async fn seed_retained_commands(
    store: &Store,
    snapshot: &StationSnapshot,
    binding: &SetVariableReference201,
) -> Vec<RequestId> {
    let mut expected = Vec::new();
    for index in 0..113 {
        let id = if index < 110 {
            format!("a-retained-{index:03}")
        } else {
            format!("z-dispatched-{:03}", index - 110)
        };
        let command = variables(snapshot, &id, std::slice::from_ref(binding)).admit(Clock.now());
        expected.push(command.request_id.clone());
        let mut write = AtomicStoreWrite::empty();
        write.command = Some(command.clone());
        // Even early IDs deliberately have no result row. Recovery must not pull a later
        // dispatched result into their selected command page to fill the result LIMIT.
        if index >= 110 {
            write.command_result = Some(persisted_result(&command, CommandLifecycle::Dispatched));
        } else if index % 2 == 1 {
            write.command_result = Some(persisted_result(
                &command,
                CommandLifecycle::TransmissionUncertain {
                    detail: "prior unresolved transmission".into(),
                },
            ));
        }
        store.write_atomic(write).await.unwrap();
    }
    expected
}

async fn assert_first_storage_page(store: &Store, expected: &[RequestId]) {
    let first = store
        .recover(RecoveryQuery {
            after_command: None,
            limit: PageLimit::new(100).unwrap(),
        })
        .await
        .unwrap();
    assert!(first.has_more);
    assert_eq!(
        first
            .active_commands
            .iter()
            .map(|c| c.request_id.clone())
            .collect::<Vec<_>>(),
        expected[..100]
    );
    assert_eq!(first.command_results.len(), 50);
    assert_eq!(
        first
            .command_results
            .iter()
            .map(|r| r.return_route.request_id.clone())
            .collect::<Vec<_>>(),
        expected[..100]
            .iter()
            .enumerate()
            .filter(|(index, _)| index % 2 == 1)
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>()
    );
}

async fn assert_coordinator_recovery_pairs(coordinator: &Coordinator, expected: &[RequestId]) {
    let mut after = None;
    let mut seen = Vec::new();
    let mut page_sizes = Vec::new();
    loop {
        let page = coordinator
            .recover_unresolved(after.clone(), PageLimit::new(100).unwrap())
            .await
            .unwrap();
        page_sizes.push(page.commands.len());
        if page.commands.is_empty() {
            break;
        }
        for (offset, recovered) in page.commands.iter().enumerate() {
            let index = seen.len() + offset;
            assert_eq!(recovered.command.request_id, expected[index]);
            assert_eq!(
                recovered.result.return_route,
                recovered.command.return_route()
            );
            assert_eq!(recovered.result.resource, recovered.command.resource);
            assert_eq!(
                recovered.result.correlation_id,
                recovered.command.correlation_id
            );
            if index < 110 && index % 2 == 0 {
                assert_eq!(recovered.result.lifecycle, CommandLifecycle::Admitted);
            } else {
                assert!(matches!(
                    recovered.result.lifecycle,
                    CommandLifecycle::TransmissionUncertain { .. }
                ));
            }
        }
        let next = page.commands.last().unwrap().command.request_id.clone();
        assert!(
            after
                .as_ref()
                .is_none_or(|prior| prior.as_str() < next.as_str())
        );
        after = Some(next);
        seen.extend(
            page.commands
                .into_iter()
                .map(|item| item.command.request_id),
        );
    }
    assert_eq!(seen.as_slice(), expected);
    assert_eq!(page_sizes, [100, 13, 0]);
}

async fn assert_tail_storage_page(store: &Store, expected: &[RequestId]) {
    let tail = store
        .recover(RecoveryQuery {
            after_command: Some(expected[99].clone()),
            limit: PageLimit::new(100).unwrap(),
        })
        .await
        .unwrap();
    assert!(!tail.has_more);
    assert_eq!(
        tail.active_commands
            .iter()
            .map(|c| c.request_id.clone())
            .collect::<Vec<_>>(),
        expected[100..]
    );
    assert!(
        tail.command_results
            .iter()
            .all(|r| expected[100..].contains(&r.return_route.request_id))
    );
}

async fn assert_reopened_recovery(restarted: &Coordinator, expected: &[RequestId]) {
    let mut after = None;
    let mut reopened_seen = Vec::new();
    loop {
        let page = restarted
            .recover_unresolved(after.clone(), PageLimit::new(100).unwrap())
            .await
            .unwrap();
        if page.commands.is_empty() {
            break;
        }
        assert!(
            page.commands
                .iter()
                .all(|item| !matches!(item.result.lifecycle, CommandLifecycle::Dispatched))
        );
        after = page
            .commands
            .last()
            .map(|item| item.command.request_id.clone());
        reopened_seen.extend(
            page.commands
                .into_iter()
                .map(|item| item.command.request_id),
        );
    }
    assert_eq!(reopened_seen.as_slice(), expected);
}

async fn assert_recovery_did_not_send(running: &mut RunningSession) {
    // A real socket sentinel proves that neither recovery pass queued a native write.
    let pending = running
        .handle
        .try_call(OutboundCall {
            message_id: "recovery-sentinel".into(),
            action: ProtocolActionName::new("Heartbeat").unwrap(),
            payload: serde_json::json!({}),
            correlation_id: CorrelationId::new("recovery-sentinel").unwrap(),
        })
        .unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        serde_json::json!([2, "recovery-sentinel", "Heartbeat", {}])
    );
    running
        .peer
        .send_text(
            serde_json::json!([3,"recovery-sentinel",{"currentTime":"2026-09-01T02:00:00Z"}])
                .to_string(),
        )
        .await
        .unwrap();
    assert!(matches!(
        pending.receive().await,
        SessionCallOutcome::Result { .. }
    ));
}
