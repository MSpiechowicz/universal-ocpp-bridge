use super::*;
use uob_protocol_adapter::command_registry::{command_schemas, validate_privileged_operation};

#[tokio::test]
async fn profile_shape_precision_period_and_scope_fail_closed_before_wire() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    assert_invalid_set_profiles(&mut running, &state, &coordinator).await;
    assert_invalid_clear_scopes(&mut running, &state, &coordinator).await;
    // The exact connector filter needs no station authority; its remaining filters AND together.
    accepted(
        &exchange(
            &mut running,
            &coordinator,
            request(
                &state,
                "scoped-clear",
                "ClearChargingProfile",
                json!({"connectorId":1,"chargingProfilePurpose":"TxDefaultProfile","stackLevel":2}),
                false,
            ),
            json!({"status":"Accepted"}),
        )
        .await,
    );
    accepted(
        &exchange(
            &mut running,
            &coordinator,
            request(&state, "clear-all", "ClearChargingProfile", json!({}), true),
            json!({"status":"Accepted"}),
        )
        .await,
    );
    finish(running, store).await;
}

fn invalid_set_profile_payloads() -> Vec<Value> {
    let mut invalid = Vec::new();
    for rate in [
        "-0.1",
        "1.01",
        "900719925474099.11",
        "79228162514264337593543950335.1",
        "1e1000",
    ] {
        invalid.push(set_payload(1, rate, "A"));
    }
    for (path, value) in [
        ("/connectorId", json!(0)),
        (
            "/csChargingProfiles/chargingProfileId",
            json!(i64::from(i32::MAX) + 1),
        ),
        ("/csChargingProfiles/stackLevel", json!(-1)),
        ("/csChargingProfiles/chargingProfileKind", json!("Absolute")),
        (
            "/csChargingProfiles/chargingSchedule/chargingSchedulePeriod/0/startPeriod",
            json!(1),
        ),
        (
            "/csChargingProfiles/chargingSchedule/chargingSchedulePeriod/1/startPeriod",
            json!(0),
        ),
        (
            "/csChargingProfiles/chargingSchedule/chargingSchedulePeriod/0/numberPhases",
            json!(0),
        ),
        (
            "/csChargingProfiles/chargingSchedule/chargingRateUnit",
            json!("kW"),
        ),
        (
            "/csChargingProfiles/chargingSchedule/minChargingRate",
            json!(8.11),
        ),
        ("/csChargingProfiles/validFrom", json!(null)),
        (
            "/csChargingProfiles/chargingProfilePurpose",
            json!("ChargePointMaxProfile"),
        ),
    ] {
        let mut payload = set_payload(1, "0", "A");
        *payload.pointer_mut(path).unwrap() = value;
        invalid.push(payload);
    }
    let mut too_many = set_payload(1, "0", "A");
    too_many["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"] = json!(
        (0..1025)
            .map(|index| json!({"startPeriod":index,"limit":0}))
            .collect::<Vec<_>>()
    );
    invalid.push(too_many);
    let mut empty = set_payload(1, "0", "A");
    empty["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"] = json!([]);
    invalid.push(empty);
    let mut extra = set_payload(1, "0", "A");
    extra["csChargingProfiles"]["opaque"] = json!({"x":1});
    invalid.push(extra);
    let mut illegal_transaction = set_payload(1, "0", "A");
    illegal_transaction["csChargingProfiles"]["transactionId"] = json!(117);
    invalid.push(illegal_transaction);
    invalid
}

async fn assert_invalid_set_profiles(
    running: &mut RunningSession,
    state: &StationSnapshot,
    coordinator: &Arc<Coordinator>,
) {
    let invalid = invalid_set_profile_payloads();
    for (index, payload) in invalid.into_iter().enumerate() {
        let request = request(
            state,
            &format!("invalid-{index}"),
            "SetChargingProfile",
            payload,
            false,
        );
        let CommandOperation::Ocpp(operation) = &request.request.operation else {
            unreachable!();
        };
        assert_eq!(
            validate_privileged_operation(&request.request.resource, operation),
            Err(CommandErrorCode::InvalidParameters)
        );
        rejected(&coordinator.submit(request).await.unwrap());
        no_call(running).await;
    }
}

async fn assert_invalid_clear_scopes(
    running: &mut RunningSession,
    state: &StationSnapshot,
    coordinator: &Arc<Coordinator>,
) {
    for (index, payload) in [
        json!({}),
        json!({"id":-117,"connectorId":1}),
        json!({"connectorId":0}),
        json!({"connectorId":2}),
        json!({"chargingProfilePurpose":"TxProfile"}),
    ]
    .into_iter()
    .enumerate()
    {
        let request = request(
            state,
            &format!("scope-{index}"),
            "ClearChargingProfile",
            payload,
            false,
        );
        let CommandOperation::Ocpp(operation) = &request.request.operation else {
            unreachable!();
        };
        assert_eq!(
            validate_privileged_operation(&request.request.resource, operation),
            Err(CommandErrorCode::InvalidParameters)
        );
        rejected(&coordinator.submit(request).await.unwrap());
        no_call(running).await;
    }
}

#[tokio::test]
async fn optins_and_privileged_permission_remain_independent_from_canonical_limit() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut state, port, coordinator) = configured(&store, &running).await;
    let denied = scoped(coordinator.clone(), &state, vec![AccessPermission::Control]);
    assert_eq!(
        denied
            .submit(request(
                &state,
                "denied",
                "ClearChargingProfile",
                json!({}),
                true
            ))
            .await
            .unwrap_err()
            .code(),
        CommandAdmissionErrorCode::Unauthorized
    );
    no_call(&mut running).await;
    state.capabilities.operations.retain(|capability| !matches!(&capability.operation,Operation::ProtocolAction {action,..} if action=="ClearChargingProfile"));
    port.update_committed(state.clone()).unwrap();
    let result = coordinator
        .submit(request(
            &state,
            "disabled-clear",
            "ClearChargingProfile",
            json!({}),
            true,
        ))
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
    no_call(&mut running).await;
    let descriptors = command_schemas(&state);
    assert!(
        descriptors
            .iter()
            .any(|descriptor| descriptor.resource == state.station
                && descriptor.action == "SetChargingProfile")
    );
    assert!(
        !descriptors
            .iter()
            .any(|descriptor| descriptor.resource == state.station
                && descriptor.action == "ClearChargingProfile")
    );
    assert!(
        descriptors
            .iter()
            .filter(|descriptor| descriptor.action == "SetChargingProfile")
            .all(|descriptor| descriptor
                .fields
                .iter()
                .any(|field| field.name.contains("chargingSchedulePeriod[].limit")))
    );
    finish(running, store).await;
}
