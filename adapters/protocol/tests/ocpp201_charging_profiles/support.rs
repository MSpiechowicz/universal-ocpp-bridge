use super::*;
pub async fn configured(
    store: &Store,
    running: &RunningSession,
) -> (StationSnapshot, Arc<RemoteControlSession>, Arc<Coordinator>) {
    let (mut state, _, port, coordinator) = setup_device(store, running.handle.clone()).await;
    for capabilities in std::iter::once(&mut state.capabilities).chain(
        state
            .resources
            .iter_mut()
            .map(|entry| &mut entry.capabilities),
    ) {
        for action in ["SetChargingProfile", "ClearChargingProfile"] {
            capabilities.operations.push(SupportedOperation {
                operation: Operation::ProtocolAction {
                    protocol: ProtocolEdition::Ocpp201,
                    action: action.to_owned(),
                },
                parameters: vec![],
            });
        }
    }
    port.update_committed(state.clone()).unwrap();
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(state.clone());
    store.write_atomic(write).await.unwrap();
    (state, port, coordinator)
}
pub fn profile(rate: &str, unit: &str) -> Value {
    let mut payload = json!({"evseId":1,"chargingProfile":{"id":i32::MIN,"stackLevel":2,
        "chargingProfilePurpose":"TxDefaultProfile","chargingProfileKind":"Recurring","recurrencyKind":"Daily",
        "validFrom":"2026-09-01T01:00:00Z","validTo":"2026-09-02T01:00:00Z",
        "chargingSchedule":[{"id":i32::MAX,"duration":0,"startSchedule":"2026-09-01T01:00:00Z",
            "chargingRateUnit":unit,"chargingSchedulePeriod":[{"startPeriod":0,"limit":0,"numberPhases":1},
                {"startPeriod":86401,"limit":0}],"minChargingRate":0}]}});
    payload["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"] =
        serde_json::from_str(rate).unwrap();
    payload
}
pub fn request(
    state: &StationSnapshot,
    id: &str,
    action: &str,
    payload: Value,
    station: bool,
) -> ExternalCommand<Value> {
    let mut request = command(state, id, protocol(action, payload));
    if station {
        request.request.resource = state.station.clone();
    }
    request
}
pub async fn exchange(
    running: &mut RunningSession,
    coordinator: &Arc<Coordinator>,
    request: ExternalCommand<Value>,
    reply: Value,
) -> CommandResult {
    let coordinator = coordinator.clone();
    let pending = tokio::spawn(async move { coordinator.submit(request).await.unwrap() });
    let call = receive_json(&mut running.peer).await;
    assert_eq!(call[0], 2);
    running
        .peer
        .send_text(json!([3, call[1], reply]).to_string())
        .await
        .unwrap();
    pending.await.unwrap()
}
pub async fn baseline(
    running: &mut RunningSession,
    state: &StationSnapshot,
    coordinator: &Arc<Coordinator>,
) {
    for (index, purpose) in ["ChargingStationMaxProfile", "TxDefaultProfile", "TxProfile"]
        .into_iter()
        .enumerate()
    {
        let result = exchange(
            running,
            coordinator,
            request(
                state,
                &format!("baseline-{index}"),
                "ClearChargingProfile",
                json!({"chargingProfileCriteria":{"chargingProfilePurpose":purpose}}),
                true,
            ),
            json!({"status":"Unknown"}),
        )
        .await;
        assert!(matches!(
            result.charging_profile_201,
            Some(ChargingProfileResult201::ClearChargingProfile {
                status: ClearChargingProfileStatus201::Unknown,
                ..
            })
        ));
    }
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
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    assert_eq!(result.charging_profile_201, None);
}
