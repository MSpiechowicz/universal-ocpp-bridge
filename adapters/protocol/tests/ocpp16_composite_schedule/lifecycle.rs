use super::*;

#[tokio::test]
async fn delayed_schedule_reply_keeps_inbound_heartbeat_progress_and_duplicate_is_one_shot() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let request = query(&state, "slow-schedule", 1, None);
    let submit = {
        let coordinator = coordinator.clone();
        let request = request.clone();
        tokio::spawn(async move { coordinator.submit(request).await.unwrap() })
    };
    assert_eq!(receive_json(&mut running.peer).await[1], "slow-schedule");
    running
        .peer
        .send_text(json!([2, "heartbeat-during-query", "Heartbeat", {}]).to_string())
        .await
        .unwrap();
    let incoming = timeout(Duration::from_secs(1), running.outputs.incoming.receive())
        .await
        .unwrap()
        .unwrap();
    incoming
        .responder
        .respond(&json!({"currentTime":"2026-09-01T02:00:00Z"}))
        .unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([3,"heartbeat-during-query",{"currentTime":"2026-09-01T02:00:00Z"}])
    );
    assert!(!submit.is_finished());
    let duplicate = coordinator.submit(request.clone()).await.unwrap();
    assert_eq!(duplicate.lifecycle, CommandLifecycle::Dispatched);
    assert!(
        timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running
        .peer
        .send_text(json!([3, "slow-schedule", reply("0", "A")]).to_string())
        .await
        .unwrap();
    let result = submit.await.unwrap();
    accepted(&result);
    assert_eq!(coordinator.submit(request).await.unwrap(), result);
    durable(&store, &result).await;
    assert!(
        timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    finish(running, store).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep each original socket, terminal result and restart together.
async fn timeout_disconnect_callerror_and_late_reply_never_replay_after_restart() {
    for mode in ["timeout", "disconnect", "callerror"] {
        let mut running = session("ocpp1.6", Duration::from_millis(120)).await;
        let database = Database::new();
        let store = database.open();
        let (state, _, coordinator) = configured(&store, &running).await;
        let request = query(&state, mode, 1, None);
        let submit = {
            let coordinator = coordinator.clone();
            let request = request.clone();
            tokio::spawn(async move { coordinator.submit(request).await.unwrap() })
        };
        assert_eq!(receive_json(&mut running.peer).await[1], mode);
        match mode {
            "disconnect" => running.peer.disconnect().await.unwrap(),
            "callerror" => running
                .peer
                .send_text(
                    json!([4, mode, "NotSupported", "private native detail", {}]).to_string(),
                )
                .await
                .unwrap(),
            _ => {}
        }
        let result = submit.await.unwrap();
        if mode == "callerror" {
            assert!(matches!(
                result.lifecycle,
                CommandLifecycle::ProtocolResponse {
                    accepted: false,
                    error: Some(CommandError {
                        code: CommandErrorCode::ProtocolRejected,
                        ..
                    })
                }
            ));
        } else {
            assert!(matches!(
                result.lifecycle,
                CommandLifecycle::TransmissionUncertain { .. }
            ));
        }
        assert!(result.composite_schedule_16.is_none());
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private native detail")
        );
        if mode == "timeout" {
            running
                .peer
                .send_text(json!([3, mode, reply("1", "A")]).to_string())
                .await
                .unwrap();
            timeout(Duration::from_secs(1), running.outputs.diagnostics.recv())
                .await
                .unwrap()
                .unwrap();
            durable(&store, &result).await;
        }
        if mode == "disconnect" {
            running.task.wait().await.unwrap();
            running.server.abort();
            store.shutdown(Duration::from_secs(1)).await.unwrap();
        } else {
            finish(running, store).await;
        }
        drop(coordinator);
        let store = database.open();
        let mut next = session("ocpp1.6", Duration::from_secs(1)).await;
        let (next_state, _, coordinator) = configured(&store, &next).await;
        let recovered = coordinator
            .recover_unresolved(PageLimit::new(100).unwrap())
            .await
            .unwrap();
        assert_eq!(recovered.commands.len(), usize::from(mode != "callerror"));
        assert_eq!(coordinator.submit(request).await.unwrap(), result);
        assert!(
            timeout(Duration::from_millis(40), next.peer.receive())
                .await
                .is_err()
        );
        // A stale reply on the new socket cannot become evidence for this old request.
        next.peer
            .send_text(json!([3, mode, reply("2", "A")]).to_string())
            .await
            .unwrap();
        timeout(Duration::from_secs(1), next.outputs.diagnostics.recv())
            .await
            .unwrap()
            .unwrap();
        durable(&store, &result).await;
        let fresh = exchange(
            &mut next,
            &coordinator,
            query(&next_state, "explicit-new-query", 1, None),
            reply("3", "W"),
        )
        .await;
        accepted(&fresh);
        assert_eq!(
            fresh
                .composite_schedule_16
                .as_ref()
                .unwrap()
                .charging_schedule
                .as_ref()
                .unwrap()
                .charging_schedule_period[0]
                .limit,
            ExactDecimal::new(3, 0)
        );
        finish(next, store).await;
    }
}

#[tokio::test]
async fn interrupted_dispatch_recovers_uncertain_without_resending_schedule() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let request = query(&state, "interrupted-query", 0, None);
    let submit = {
        let coordinator = coordinator.clone();
        let request = request.clone();
        tokio::spawn(async move { coordinator.submit(request).await.unwrap() })
    };
    receive_json(&mut running.peer).await;
    submit.abort();
    let _ = submit.await;
    assert_eq!(
        store
            .command_result_by_request_id(request.request.request_id.clone())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        CommandLifecycle::Dispatched
    );
    finish(running, store).await;
    drop(coordinator);
    let store = database.open();
    let mut next = session("ocpp1.6", Duration::from_secs(1)).await;
    let (_, _, coordinator) = configured(&store, &next).await;
    let recovered = coordinator
        .recover_unresolved(PageLimit::new(100).unwrap())
        .await
        .unwrap();
    assert_eq!(recovered.commands.len(), 1);
    let result = &recovered.commands[0].result;
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert!(result.composite_schedule_16.is_none());
    assert_eq!(coordinator.submit(request).await.unwrap(), *result);
    assert!(
        timeout(Duration::from_millis(40), next.peer.receive())
            .await
            .is_err()
    );
    finish(next, store).await;
}
