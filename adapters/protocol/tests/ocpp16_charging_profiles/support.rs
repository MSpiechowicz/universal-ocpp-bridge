use super::*;

pub async fn configured(
    store: &Store,
    running: &RunningSession,
) -> (StationSnapshot, Arc<RemoteControlSession>, Arc<Coordinator>) {
    let (mut state, _, port, coordinator) = setup(store, running.handle.clone()).await;
    for action in ["SetChargingProfile", "ClearChargingProfile"] {
        let capability = SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: action.to_owned(),
            },
            parameters: vec![],
        };
        state.capabilities.operations.push(capability.clone());
        for resource in &mut state.resources {
            resource.capabilities.operations.push(capability.clone());
        }
    }
    port.update_committed(state.clone()).unwrap();
    (state, port, coordinator)
}

pub fn set_payload(connector: i32, rate: &str, unit: &str) -> Value {
    let mut payload = json!({"connectorId":connector,"csChargingProfiles":{
        "chargingProfileId":-117,"stackLevel":2,"chargingProfilePurpose":"TxDefaultProfile",
        "chargingProfileKind":"Recurring","recurrencyKind":"Daily",
        "validFrom":"2026-09-01T01:00:00Z","validTo":"2026-09-02T01:00:00Z",
        "chargingSchedule":{"duration":60,"startSchedule":"2026-09-01T01:00:00Z",
            "chargingRateUnit":unit,"chargingSchedulePeriod":[{"startPeriod":0,"limit":0,"numberPhases":4},{"startPeriod":86401,"limit":0}],"minChargingRate":8.1}
    }});
    payload["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"] =
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
    assert!(matches!(
        call[2].as_str(),
        Some("SetChargingProfile" | "ClearChargingProfile")
    ));
    running
        .peer
        .send_text(json!([3, call[1], reply]).to_string())
        .await
        .unwrap();
    pending.await.unwrap()
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
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::InvalidParameters,
                ..
            }
        }
    ));
    assert!(result.charging_profile_16.is_none());
}
