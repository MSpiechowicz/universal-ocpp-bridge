#![allow(dead_code)] // Shared remote-control fixture serves other action suites too.
mod endpoint_support;
#[path = "ocpp16_charging_profiles/lifecycle.rs"]
mod lifecycle;
#[path = "ocpp16_charging_profiles/precision.rs"]
mod precision;
#[path = "ocpp16_charging_profiles/support.rs"]
mod profile_support;
#[path = "ocpp16_remote_control/support.rs"]
mod support;
#[path = "ocpp16_charging_profiles/transactions.rs"]
mod transactions;
#[path = "ocpp16_charging_profiles/validation.rs"]
mod validation;

use profile_support::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::time::timeout;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::remote_control::RemoteControlSession;

#[tokio::test]
async fn exact_native_profiles_and_action_specific_statuses_survive_reopen() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let before = persisted(&store).await;
    let mut results = Vec::new();
    results.extend(exact_set_profile_results(&mut running, &state, &coordinator).await);
    results.extend(clear_profile_status_results(&mut running, &state, &coordinator).await);
    assert_eq!(
        persisted(&store).await,
        before,
        "acknowledgements do not mutate observed charging state"
    );
    drop(coordinator);
    finish(running, store).await;
    let reopened = database.open();
    for result in results {
        assert_eq!(
            reopened
                .command_result_by_request_id(result.return_route.request_id.clone())
                .await
                .unwrap(),
            Some(result)
        );
    }
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}

async fn exact_set_profile_results(
    running: &mut RunningSession,
    state: &StationSnapshot,
    coordinator: &Arc<Coordinator>,
) -> Vec<CommandResult> {
    let mut results = Vec::new();
    for (index, (rate, unit, status)) in [
        ("900719925474099.1", "A", "Accepted"),
        ("0", "W", "Rejected"),
        ("79228162514264337593543950335", "W", "NotSupported"),
        ("8.100000e0", "A", "Accepted"),
    ]
    .into_iter()
    .enumerate()
    {
        let result = exchange(
            running,
            coordinator,
            request(
                state,
                &format!("exact-{index}"),
                "SetChargingProfile",
                set_payload(1, rate, unit),
                false,
            ),
            json!({"status":status}),
        )
        .await;
        assert_eq!(
            result.schema_version,
            ContractVersion::V1_CHARGING_PROFILE_16
        );
        let ChargingProfileResult16::SetChargingProfile {
            request,
            status: native_status,
        } = result.charging_profile_16.as_ref().unwrap()
        else {
            panic!("set evidence");
        };
        assert_eq!(serde_json::to_value(native_status).unwrap(), status);
        assert_eq!(request.connector_id, 1);
        assert_eq!(request.cs_charging_profiles.charging_profile_id, -117);
        assert_eq!(
            request.cs_charging_profiles.recurrency_kind,
            Some(ChargingProfileRecurrency16::Daily)
        );
        let schedule = &request.cs_charging_profiles.charging_schedule;
        assert_eq!(
            schedule.charging_schedule_period[0].limit.to_string(),
            [
                "900719925474099.1",
                "0",
                "79228162514264337593543950335",
                "8.1"
            ][index]
        );
        assert_eq!(schedule.charging_schedule_period[0].number_phases, Some(4));
        assert_eq!(schedule.charging_schedule_period[1].start_period, 86401);
        assert_eq!(schedule.duration, Some(60));
        assert_eq!(schedule.min_charging_rate, Some(ExactDecimal::new(81, 1)));
        assert_eq!(
            schedule.charging_rate_unit,
            if unit == "A" {
                CompositeScheduleRateUnit16::A
            } else {
                CompositeScheduleRateUnit16::W
            }
        );
        assert_eq!(
            result.charging_profile_16.as_ref().unwrap().accepted(),
            status == "Accepted"
        );
        assert!(result.observed_effects.is_empty());
        results.push(result);
    }
    results
}

async fn clear_profile_status_results(
    running: &mut RunningSession,
    state: &StationSnapshot,
    coordinator: &Arc<Coordinator>,
) -> Vec<CommandResult> {
    let mut results = Vec::new();
    for (index, status) in ["Accepted", "Unknown"].into_iter().enumerate() {
        let result = exchange(
            running,
            coordinator,
            request(
                state,
                &format!("clear-{index}"),
                "ClearChargingProfile",
                json!({
                    "id": -117,
                    "connectorId": 999,
                    "chargingProfilePurpose": "TxProfile",
                    "stackLevel": 3,
                }),
                true,
            ),
            json!({"status": status}),
        )
        .await;
        let ChargingProfileResult16::ClearChargingProfile {
            request,
            status: native_status,
        } = result.charging_profile_16.as_ref().unwrap()
        else {
            panic!("clear evidence");
        };
        assert_eq!(serde_json::to_value(native_status).unwrap(), status);
        assert_eq!(request.id, Some(-117));
        assert_eq!(request.connector_id, Some(999));
        results.push(result);
    }
    results
}

#[tokio::test]
async fn independent_profile_fixtures_reach_native_status_boundary_without_station_widening() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let wire = fixture("profile-recurring-default");
    let wrong = request(
        &state,
        "fixture-wrong-child",
        "SetChargingProfile",
        wire[3].clone(),
        false,
    );
    rejected(&coordinator.submit(wrong).await.unwrap());
    no_call(&mut running).await;
    let mut valid = request(
        &state,
        "fixture-right-child",
        "SetChargingProfile",
        wire[3].clone(),
        false,
    );
    valid.request.resource = state
        .resources
        .iter()
        .find(|entry| {
            matches!(
                entry.resource.native_protocol_reference,
                Some(NativeProtocolReference::Ocpp16 { connector_id: 2 })
            )
        })
        .unwrap()
        .resource
        .clone();
    let result = exchange(
        &mut running,
        &coordinator,
        valid,
        fixture("profile-set-accepted")[2].clone(),
    )
    .await;
    accepted(&result);
    let ChargingProfileResult16::SetChargingProfile {
        request: set_request,
        ..
    } = result.charging_profile_16.unwrap()
    else {
        panic!("set evidence");
    };
    assert_eq!(set_request.connector_id, 2);
    assert_eq!(
        set_request.cs_charging_profiles.charging_profile_id,
        i32::MAX
    );
    assert_eq!(
        set_request
            .cs_charging_profiles
            .charging_schedule
            .charging_schedule_period[1]
            .start_period,
        604_801
    );
    let cleared = exchange(
        &mut running,
        &coordinator,
        request(
            &state,
            "fixture-id-clear",
            "ClearChargingProfile",
            fixture("profile-clear-id-overrides")[3].clone(),
            true,
        ),
        fixture("profile-clear-accepted")[2].clone(),
    )
    .await;
    let ChargingProfileResult16::ClearChargingProfile { request, status } =
        cleared.charging_profile_16.unwrap()
    else {
        panic!("clear evidence");
    };
    assert_eq!(request.id, Some(-117));
    assert_eq!(request.connector_id, Some(999));
    assert_eq!(status, ClearChargingProfileStatus16::Accepted);
    finish(running, store).await;
}
