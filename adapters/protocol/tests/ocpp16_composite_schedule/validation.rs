use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep invalid admission cases and the no-wire assertion together.
async fn invalid_schedule_requests_and_insufficient_authority_send_no_call() {
    let mut running = session("ocpp1.6", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (state, port, coordinator) = configured(&store, &running).await;
    for (index, payload) in [
        json!({"connectorId":0,"duration":60}),
        json!({"connectorId":1,"duration":0}),
        json!({"connectorId":1,"duration":-1}),
        json!({"connectorId":1,"duration":1.1}),
        json!({"connectorId":1,"duration":2_147_483_648_u64}),
        json!({"connectorId":1,"duration":null}),
        json!({"duration":60}),
        json!({"connectorId":1}),
        json!({"connectorId":1,"duration":60,"chargingRateUnit":null}),
        json!({"connectorId":1,"duration":60,"chargingRateUnit":"kW"}),
        json!({"connectorId":1,"duration":60,"other":1}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut request = query(&state, &format!("invalid-{index}"), 1, None);
        request.request.operation = protocol("GetCompositeSchedule", payload);
        let result = coordinator.submit(request).await.unwrap();
        assert!(matches!(
            result.lifecycle,
            CommandLifecycle::Rejected {
                error: CommandError {
                    code: CommandErrorCode::InvalidParameters,
                    ..
                }
            }
        ));
        assert!(result.composite_schedule_16.is_none());
    }
    for (index, permissions) in [
        vec![AccessPermission::Read],
        vec![AccessPermission::Control],
    ]
    .into_iter()
    .enumerate()
    {
        let commands = scoped(coordinator.clone(), &state, permissions);
        assert_eq!(
            commands
                .submit(query(&state, &format!("unauthorized-{index}"), 1, None))
                .await
                .unwrap_err()
                .code(),
            CommandAdmissionErrorCode::Unauthorized
        );
    }
    let mut foreign = query(&state, "foreign-schema", 1, None);
    if let CommandOperation::Ocpp(operation) = &mut foreign.request.operation {
        operation.payload_schema = PayloadSchemaId::new("foreign").unwrap();
    }
    let result = coordinator.submit(foreign).await.unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::InvalidParameters,
                ..
            }
        }
    ));
    let mut pending = state.clone();
    pending
        .current_values
        .retain(|value| value.point_id.as_str() != "ocpp16/registration/status");
    port.update_committed(pending).unwrap();
    let result = coordinator
        .submit(query(&state, "unregistered", 1, None))
        .await
        .unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));
    port.update_committed(state.clone()).unwrap();
    let mut expired = query(&state, "expired-query", 1, None);
    expired.request.expires_at = Clock.now();
    let result = coordinator.submit(expired).await.unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::Expired,
                ..
            }
        }
    ));
    let mut no_capability = state.clone();
    no_capability.resources[0]
        .capabilities
        .operations
        .retain(|capability| {
            capability.operation
                != (Operation::ProtocolAction {
                    protocol: ProtocolEdition::Ocpp16j,
                    action: "GetCompositeSchedule".to_owned(),
                })
        });
    port.update_committed(no_capability).unwrap();
    let result = coordinator
        .submit(query(&state, "not-advertised", 1, None))
        .await
        .unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::UnsupportedOperation,
                ..
            }
        }
    ));
    assert!(
        timeout(Duration::from_millis(50), running.peer.receive())
            .await
            .is_err()
    );
    finish(running, store).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One socket scenario observes the complete hostile-reply matrix.
async fn malformed_native_schedule_is_uncertain_without_fabricated_evidence() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let mut invalid = vec![
        json!({"status":"Accepted"}),
        json!({"status":"Unsupported"}),
        json!({"status":"Rejected","connectorId":null}),
        json!({"status":"Rejected","other":true}),
    ];
    for (pointer, value) in [
        ("/scheduleStart", json!(null)),
        ("/scheduleStart", json!("invalid")),
        ("/scheduleStart", json!("9999-12-31T23:59:59Z")),
        ("/chargingSchedule", json!(null)),
        ("/connectorId", json!(2)),
        ("/chargingSchedule/duration", json!(0)),
        ("/chargingSchedule/duration", json!(61)),
        ("/chargingSchedule/duration", json!(null)),
        ("/chargingSchedule/duration", json!(1.5)),
        ("/chargingSchedule/startSchedule", json!("invalid")),
        ("/chargingSchedule/chargingRateUnit", json!("W")),
        ("/chargingSchedule/chargingSchedulePeriod", json!([])),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":1,"limit":0}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0},{"startPeriod":0,"limit":1}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0},{"startPeriod":30,"limit":1},{"startPeriod":20,"limit":1}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0},{"startPeriod":60,"limit":1}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0,"numberPhases":0}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0,"numberPhases":-1}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0,"numberPhases":null}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0,"numberPhases":2_147_483_648_u64}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0,"other":1}]),
        ),
        ("/chargingSchedule/minChargingRate", json!(-1)),
        ("/chargingSchedule/minChargingRate", json!(0.01)),
        ("/chargingSchedule/minChargingRate", json!(null)),
        ("/chargingSchedule/minChargingRate", json!("1")),
    ] {
        let mut payload = reply("0", "A");
        // Optional members are inserted through their parent because JSON pointers cannot insert.
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        payload
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.to_owned(), value);
        invalid.push(payload);
    }
    let mut unknown_schedule = reply("0", "A");
    unknown_schedule["chargingSchedule"]["other"] = json!(1);
    invalid.push(unknown_schedule);
    for rate in [
        "-1",
        "0.01",
        "\"1\"",
        "null",
        "true",
        "79228162514264337593543950336",
        "7922816251426433759354395033.6",
        "79228162514264337593543950334.1",
        "1.00000000000000000000000000001",
        "1e-29",
        "1e29",
    ] {
        invalid.push(reply(rate, "A"));
    }
    for (index, payload) in invalid.into_iter().enumerate() {
        let result = exchange(
            &mut running,
            &coordinator,
            query(&state, &format!("hostile-{index}"), 1, Some("A")),
            payload.clone(),
        )
        .await;
        assert!(
            matches!(
                result.lifecycle,
                CommandLifecycle::TransmissionUncertain { .. }
            ),
            "{payload}: {:?}",
            result.lifecycle
        );
        assert!(result.composite_schedule_16.is_none());
        assert!(result.observed_effects.is_empty());
        durable(&store, &result).await;
    }
    finish(running, store).await;
}
