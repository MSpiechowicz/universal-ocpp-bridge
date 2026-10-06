use super::support::*;
use serde_json::{Value, json};

fn status(reply: &Value) -> (Value, Value) {
    assert_eq!(reply[0], 3);
    (
        reply[2]["status"].clone(),
        reply[2]["statusInfo"]["reasonCode"].clone(),
    )
}

#[tokio::test]
async fn charging_needs_and_ev_schedules_follow_transactions_policy_and_the_bridges_own_limit() {
    let fixture = fixture();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connect(&fixture, &client, "station-a").await;

    let early = notify(&mut socket, "ev-charging-needs-ac", |_| {}).await;
    assert_eq!(status(&early), (json!("Rejected"), json!("TxNotFound")));
    let unknown = notify(&mut socket, "ev-charging-needs-dc", |payload| {
        payload["evseId"] = json!(7);
    })
    .await;
    assert_eq!(status(&unknown), (json!("Rejected"), json!("UnknownEvse")));

    started(&mut socket, "tx-121").await;
    let needs = notify(&mut socket, "ev-charging-needs-ac", |_| {}).await;
    assert_eq!(
        needs[2],
        corpus("ev-charging-needs-processing")[2],
        "the operator EMS answers later through a TxProfile (K15.FR.05)"
    );
    let snapshot = fixture.snapshot(&client, "station-a").await;
    let needs_point = |name: &str| {
        point(
            &snapshot,
            true,
            &format!("ocpp201/evse-1/ev-charging-needs/{name}"),
        )
    };
    assert_eq!(needs_point("status"), text("Processing"));
    assert_eq!(needs_point("transaction_id"), text("tx-121"));
    assert_eq!(
        needs_point("requested_energy_transfer"),
        text("AC_three_phase")
    );
    assert_eq!(needs_point("departure_time"), text("2026-10-06T18:30:00Z"));
    assert_eq!(
        needs_point("ev_max_current_a"),
        json!({"type":"unsigned_integer","value":32})
    );
    no_call(&mut socket).await;

    let unconstrained = notify(&mut socket, "ev-charging-schedule", |_| {}).await;
    assert_eq!(
        unconstrained[2],
        corpus("ev-charging-schedule-accepted")[2],
        "no bridge TxProfile exists, so none can be exceeded"
    );
    let call = installed(&client, &fixture, &mut socket, limit("ems-limit-121", "16")).await;
    assert_eq!(
        call[3]["chargingProfile"]["chargingProfilePurpose"],
        "TxProfile"
    );
    assert_eq!(call[3]["chargingProfile"]["transactionId"], "tx-121");
    let within = notify(&mut socket, "ev-charging-schedule", |_| {}).await;
    assert_eq!(within[2], corpus("ev-charging-schedule-accepted")[2]);
    let exceeding = notify(&mut socket, "ev-charging-schedule", |payload| {
        payload["chargingSchedule"]["chargingSchedulePeriod"][1]["limit"] = json!(16.1);
    })
    .await;
    assert_eq!(
        exceeding[2],
        corpus("ev-charging-schedule-rejected")[2],
        "16.1 A exceeds the bridge's own 16 A TxProfile (K15.FR.12)"
    );
    let snapshot = fixture.snapshot(&client, "station-a").await;
    let schedule_point = |name: &str| {
        point(
            &snapshot,
            true,
            &format!("ocpp201/evse-1/ev-charging-schedule/{name}"),
        )
    };
    assert_eq!(schedule_point("basis"), text("exceeds_csms_schedule"));
    assert_eq!(schedule_point("reason"), text("ValueTooHigh"));
    assert_eq!(
        schedule_point("period_count"),
        json!({"type":"unsigned_integer","value":3})
    );
    assert_eq!(schedule_point("charging_rate_unit"), text("A"));
    // Renegotiation (K15.FR.13) is the operator's decision; the bridge sends nothing itself.
    no_call(&mut socket).await;

    let watts = notify(&mut socket, "ev-charging-schedule", |payload| {
        payload["chargingSchedule"]["chargingRateUnit"] = json!("W");
    })
    .await;
    assert_eq!(
        status(&watts),
        (json!("Rejected"), json!("Unspecified")),
        "amperes are never converted to watts with a guessed voltage"
    );
    let heartbeat = station_call(&mut socket, "after-negotiation", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
}

#[tokio::test]
async fn the_default_policy_never_promises_a_schedule_and_invalid_needs_are_refused() {
    let fixture = fixture();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connect(&fixture, &client, "station-b").await;
    started(&mut socket, "tx-b").await;
    let reply = notify(&mut socket, "ev-charging-needs-dc", |_| {}).await;
    assert_eq!(
        reply[2],
        corpus("ev-charging-needs-rejected")[2],
        "K15.FR.04"
    );
    let snapshot = fixture.snapshot(&client, "station-b").await;
    let needs_point = |name: &str| {
        point(
            &snapshot,
            true,
            &format!("ocpp201/evse-1/ev-charging-needs/{name}"),
        )
    };
    assert_eq!(needs_point("status"), text("Rejected"));
    assert_eq!(needs_point("reason"), text("NotEnabled"));
    assert_eq!(
        needs_point("state_of_charge_percent"),
        json!({"type":"unsigned_integer","value":35})
    );
    let invalid = notify(&mut socket, "ev-charging-needs-dc", |payload| {
        payload["chargingNeeds"]["requestedEnergyTransfer"] = json!("AC_single_phase");
    })
    .await;
    assert_eq!(invalid[0], 4);
    assert_eq!(invalid[2], "PropertyConstraintViolation", "K15.FR.06");
    assert_eq!(
        fixture.snapshot(&client, "station-b").await,
        snapshot,
        "a refused notification changes nothing"
    );
    no_call(&mut socket).await;
}
