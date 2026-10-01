use super::*;

pub async fn configured(
    store: &Store,
    running: &RunningSession,
) -> (StationSnapshot, Arc<RemoteControlSession>, Arc<Coordinator>) {
    let (mut state, _, port, coordinator) = setup(store, running.handle.clone()).await;
    let capability = SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: "GetCompositeSchedule".to_owned(),
        },
        parameters: vec![],
    };
    state.capabilities.operations.push(capability.clone());
    state.resources[0].capabilities.operations.push(capability);
    port.update_committed(state.clone()).unwrap();
    (state, port, coordinator)
}

pub fn query(
    state: &StationSnapshot,
    id: &str,
    connector: i32,
    unit: Option<&str>,
) -> ExternalCommand<Value> {
    let mut payload = json!({"connectorId":connector,"duration":60});
    if let Some(unit) = unit {
        payload["chargingRateUnit"] = json!(unit);
    }
    let mut query = command(state, id, protocol("GetCompositeSchedule", payload));
    if connector == 0 {
        query.request.resource = state.station.clone();
    }
    query
}

pub fn reply(rate: &str, unit: &str) -> Value {
    serde_json::from_str(&format!(r#"{{"status":"Accepted","scheduleStart":"2026-09-01T04:00:00+02:00","chargingSchedule":{{"chargingRateUnit":"{unit}","chargingSchedulePeriod":[{{"startPeriod":0,"limit":{rate}}}]}}}}"#)).unwrap()
}

pub async fn exchange(
    running: &mut RunningSession,
    coordinator: &Arc<Coordinator>,
    query: ExternalCommand<Value>,
    payload: Value,
) -> CommandResult {
    let expected = match &query.request.operation {
        CommandOperation::Ocpp(operation) => operation.payload.clone(),
        _ => unreachable!(),
    };
    let id = query.request.request_id.as_str().to_owned();
    let coordinator = coordinator.clone();
    let submit = tokio::spawn(async move { coordinator.submit(query).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2, id, "GetCompositeSchedule", expected])
    );
    running
        .peer
        .send_text(json!([3, id, payload]).to_string())
        .await
        .unwrap();
    submit.await.unwrap()
}

pub async fn durable(store: &Store, result: &CommandResult) {
    assert_eq!(
        store
            .command_result_by_request_id(result.return_route.request_id.clone())
            .await
            .unwrap()
            .as_ref(),
        Some(result)
    );
}

pub async fn finish(running: RunningSession, store: Store) {
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
