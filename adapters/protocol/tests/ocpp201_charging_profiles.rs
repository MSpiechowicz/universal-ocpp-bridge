#![allow(dead_code)] // Existing live-socket fixture also serves other remote families.
mod endpoint_support;
#[path = "ocpp201_charging_profiles/phase.rs"]
mod phase;
#[path = "ocpp201_charging_profiles/support.rs"]
mod profiles;
#[path = "ocpp201_remote_control/support.rs"]
mod remote;
#[path = "ocpp201_charging_profiles/validation.rs"]
mod validation;
use profiles::*;
use remote::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v201::remote_control::RemoteControlSession;

#[tokio::test]
async fn exact_native_requests_denials_redaction_and_immutable_reopen_evidence() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    baseline(&mut running, &state, &coordinator).await;
    let mut retained = Vec::new();
    for (index, (rate, unit, status)) in [
        ("900719925474099.1", "A", "Accepted"),
        ("0", "W", "Rejected"),
        ("79228162514264337593543950335", "W", "Accepted"),
        ("8.10000e0", "A", "Accepted"),
    ]
    .into_iter()
    .enumerate()
    {
        let result = exchange(&mut running, &coordinator, request(&state,&format!("exact-{index}"),
            "SetChargingProfile", profile(rate, unit), false), json!({"status":status,"statusInfo":{
                "reasonCode":"InvalidValue","additionalInfo":"opaque-private-value","customData":{"vendorId":"secret"}}})).await;
        assert_eq!(
            result.schema_version,
            ContractVersion::V1_CHARGING_PROFILE_201
        );
        let ChargingProfileResult201::SetChargingProfile {
            request,
            status: native,
            reason_code,
        } = result.charging_profile_201.as_ref().unwrap()
        else {
            panic!("set")
        };
        assert_eq!(serde_json::to_value(native).unwrap(), status);
        assert_eq!(request.evse_id, 1);
        assert_eq!(request.charging_profile.id, i32::MIN);
        let schedule = &request.charging_profile.charging_schedule[0];
        assert_eq!(schedule.id, i32::MAX);
        assert_eq!(schedule.duration, Some(0));
        assert_eq!(
            schedule.charging_schedule_period[0].limit.to_string(),
            [
                "900719925474099.1",
                "0",
                "79228162514264337593543950335",
                "8.1"
            ][index]
        );
        assert_eq!(schedule.charging_schedule_period[1].start_period, 86401);
        assert_eq!(schedule.charging_schedule_period[1].number_phases, None);
        assert_eq!(schedule.min_charging_rate, Some(ExactDecimal::new(0, 0)));
        assert_eq!(*reason_code, Some(ChargingProfileReason201::InvalidValue));
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("opaque-private-value")
        );
        assert!(result.observed_effects.is_empty());
        retained.push(result);
    }
    drop(coordinator);
    finish(running, store).await;
    let reopened = database.open();
    for result in retained {
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
