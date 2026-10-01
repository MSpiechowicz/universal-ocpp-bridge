#![allow(dead_code)] // Existing shared fixture includes other remote-control operations.
mod endpoint_support;
#[path = "ocpp16_composite_schedule/lifecycle.rs"]
mod lifecycle;
#[path = "ocpp16_composite_schedule/precision.rs"]
mod precision;
#[path = "ocpp16_composite_schedule/support.rs"]
mod schedule_support;
#[path = "ocpp16_remote_control/support.rs"]
mod support;
#[path = "ocpp16_composite_schedule/validation.rs"]
mod validation;

use schedule_support::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::time::timeout;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::remote_control::RemoteControlSession;

#[tokio::test]
#[allow(clippy::too_many_lines)] // Compare native metadata, zero, rejection and reopened evidence.
async fn native_schedule_preserves_exact_rates_metadata_scope_and_zero_after_reopen() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (state, _, coordinator) = configured(&store, &running).await;
    let before = persisted(&store).await;
    let mut payload = reply("900719925474099.1", "A");
    payload["connectorId"] = json!(1);
    payload["chargingSchedule"]["duration"] = json!(60);
    payload["chargingSchedule"]["startSchedule"] = json!("2026-09-01T02:01:00Z");
    payload["chargingSchedule"]["minChargingRate"] = serde_json::from_str("8.10").unwrap();
    payload["chargingSchedule"]["chargingSchedulePeriod"][0]["numberPhases"] = json!(4);
    payload["chargingSchedule"]["chargingSchedulePeriod"]
        .as_array_mut()
        .unwrap()
        .push(json!({"startPeriod":30,"limit":0}));
    let result = exchange(
        &mut running,
        &coordinator,
        query(&state, "high-tenths", 1, Some("A")),
        payload,
    )
    .await;
    accepted(&result);
    assert_eq!(
        result.schema_version,
        ContractVersion::V1_COMPOSITE_SCHEDULE_16
    );
    assert!(result.observed_effects.is_empty());
    let evidence = result.composite_schedule_16.as_ref().unwrap();
    assert_eq!(evidence.request.connector_id, 1);
    assert_eq!(evidence.request.duration, 60);
    assert_eq!(
        evidence.request.charging_rate_unit,
        Some(CompositeScheduleRateUnit16::A)
    );
    assert_eq!(evidence.connector_id, Some(1));
    assert_eq!(evidence.schedule_start, Some(time("2026-09-01T02:00:00Z")));
    let schedule = evidence.charging_schedule.as_ref().unwrap();
    assert_eq!(schedule.duration, Some(60));
    assert_eq!(schedule.start_schedule, Some(time("2026-09-01T02:01:00Z")));
    assert_eq!(schedule.min_charging_rate, Some(ExactDecimal::new(81, 1)));
    assert_eq!(
        schedule.charging_schedule_period[0].limit,
        ExactDecimal::new(9_007_199_254_740_991, 1)
    );
    assert_eq!(schedule.charging_schedule_period[0].number_phases, Some(4));
    assert_eq!(
        schedule.charging_schedule_period[1].limit,
        ExactDecimal::new(0, 0)
    );
    assert_eq!(schedule.charging_schedule_period[1].number_phases, None);
    durable(&store, &result).await;
    assert_eq!(persisted(&store).await, before);
    let zero = exchange(
        &mut running,
        &coordinator,
        query(&state, "grid-zero", 0, None),
        reply("0", "W"),
    )
    .await;
    accepted(&zero);
    let zero_evidence = zero.composite_schedule_16.as_ref().unwrap();
    assert_eq!(zero_evidence.request.connector_id, 0);
    assert_eq!(zero_evidence.request.charging_rate_unit, None);
    assert_eq!(zero_evidence.connector_id, None);
    assert_eq!(
        zero_evidence
            .charging_schedule
            .as_ref()
            .unwrap()
            .charging_rate_unit,
        CompositeScheduleRateUnit16::W
    );
    assert_eq!(
        zero_evidence.charging_schedule.as_ref().unwrap().duration,
        None
    );
    let rejected = exchange(
        &mut running,
        &coordinator,
        query(&state, "native-rejected", 1, None),
        json!({"status":"Rejected"}),
    )
    .await;
    assert!(matches!(
        rejected.lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            error: Some(CommandError {
                code: CommandErrorCode::ProtocolRejected,
                ..
            })
        }
    ));
    let denied = rejected.composite_schedule_16.as_ref().unwrap();
    assert_eq!(denied.status, CompositeScheduleStatus16::Rejected);
    assert_eq!(denied.charging_schedule, None);
    let mut metadata = reply("0", "W");
    metadata["status"] = json!("Rejected");
    metadata["connectorId"] = json!(1);
    let retained = exchange(
        &mut running,
        &coordinator,
        query(&state, "rejected-metadata", 1, Some("W")),
        metadata,
    )
    .await;
    let evidence = retained.composite_schedule_16.as_ref().unwrap();
    assert_eq!(evidence.status, CompositeScheduleStatus16::Rejected);
    assert_eq!(evidence.connector_id, Some(1));
    assert_eq!(
        evidence
            .charging_schedule
            .as_ref()
            .unwrap()
            .charging_schedule_period[0]
            .limit,
        ExactDecimal::new(0, 0)
    );
    for result in [&zero, &rejected, &retained] {
        durable(&store, result).await;
    }
    drop(coordinator);
    finish(running, store).await;
    let reopened = database.open();
    for result in [&result, &zero, &rejected, &retained] {
        durable(&reopened, result).await;
    }
    reopened.shutdown(Duration::from_secs(1)).await.unwrap();
}
