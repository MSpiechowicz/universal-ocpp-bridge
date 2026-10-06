use super::*;

pub async fn configured(
    store: &Store,
    running: &RunningSession,
) -> (StationSnapshot, Arc<RemoteControlSession>, Arc<Coordinator>) {
    let (state, _, port, coordinator) = setup_schedules(store, running.handle.clone()).await;
    (state, port, coordinator)
}

/// Station scope addresses the grid connection; otherwise EVSE 1.
pub fn query(
    state: &StationSnapshot,
    id: &str,
    action: &str,
    payload: Value,
    station: bool,
) -> ExternalCommand<Value> {
    let mut request = command(state, id, protocol(action, payload));
    request.request.resource = if station {
        state.station.clone()
    } else {
        state.resources[0].resource.clone()
    };
    request
}

/// Exact numeric lexemes survive only when parsed from text.
pub fn raw(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

pub fn submit(
    coordinator: &Arc<Coordinator>,
    request: ExternalCommand<Value>,
) -> tokio::task::JoinHandle<CommandResult> {
    let coordinator = coordinator.clone();
    tokio::spawn(async move { coordinator.submit(request).await.unwrap() })
}

pub async fn exchange(
    running: &mut RunningSession,
    coordinator: &Arc<Coordinator>,
    request: ExternalCommand<Value>,
    reply: Value,
) -> (Value, CommandResult) {
    let pending = submit(coordinator, request);
    let call = receive_json(&mut running.peer).await;
    assert_eq!(call[0], 2);
    running
        .peer
        .send_text(json!([3, call[1], reply]).to_string())
        .await
        .unwrap();
    (call, pending.await.unwrap())
}

/// Sends one charger-originated report CALL and returns the bridge's answer frame.
pub async fn report(running: &mut RunningSession, id: &str, payload: &Value) -> Value {
    running
        .peer
        .send_text(json!([2, id, "ReportChargingProfiles", payload]).to_string())
        .await
        .unwrap();
    receive_json(&mut running.peer).await
}

/// Waits for the report owner to record a terminal report.
pub async fn settled(store: &Store, request: &str) -> CommandResult {
    for _ in 0..200 {
        let result = store
            .command_result_by_request_id(RequestId::new(request).unwrap())
            .await
            .unwrap()
            .unwrap();
        if result
            .charging_profiles_201
            .as_ref()
            .is_some_and(|evidence| !evidence.report.pending())
        {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("report did not settle")
}

pub fn profile(id: i32, purpose: &str, stack_level: i32) -> Value {
    json!({"id":id,"stackLevel":stack_level,"chargingProfilePurpose":purpose,
        "chargingProfileKind":"Absolute","chargingSchedule":[{"id":id,
        "startSchedule":"2026-09-01T00:00:00Z","chargingRateUnit":"A",
        "chargingSchedulePeriod":[{"startPeriod":0,"limit":16}]}]})
}

pub fn fragment(
    request_id: i32,
    evse_id: i32,
    source: &str,
    tbc: bool,
    profiles: &[Value],
) -> Value {
    json!({"requestId":request_id,"chargingLimitSource":source,"tbc":tbc,"evseId":evse_id,
        "chargingProfile":profiles})
}

pub fn report_state(result: &CommandResult) -> &ChargingProfileReportState201 {
    &result.charging_profiles_201.as_ref().unwrap().report
}

pub async fn no_call(running: &mut RunningSession) {
    assert!(
        timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
}

pub async fn finish(running: RunningSession, store: Store) {
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

pub fn rejected(result: &CommandResult) {
    assert!(
        matches!(result.lifecycle, CommandLifecycle::Rejected { .. }),
        "{result:?}"
    );
    assert!(result.composite_schedule_201.is_none() && result.charging_profiles_201.is_none());
}

pub fn uncertain(result: &CommandResult) {
    assert!(
        matches!(
            result.lifecycle,
            CommandLifecycle::TransmissionUncertain { .. }
        ),
        "{result:?}"
    );
    assert!(result.composite_schedule_201.is_none() && result.charging_profiles_201.is_none());
}
