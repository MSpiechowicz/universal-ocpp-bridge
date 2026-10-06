use super::support::*;
use serde_json::json;

fn grid(snapshot: &serde_json::Value, field: &str) -> serde_json::Value {
    point(
        snapshot,
        false,
        &format!("ocpp201/charging-limit/SO/{field}"),
    )
}

fn evse(snapshot: &serde_json::Value, field: &str) -> serde_json::Value {
    point(
        snapshot,
        true,
        &format!("ocpp201/evse-1/charging-limit/EMS/{field}"),
    )
}

#[tokio::test]
async fn external_limits_are_observed_state_that_never_becomes_a_command_and_survive_restart() {
    let fixture = fixture();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = connect(&fixture, &client, "station-a").await;
    started(&mut socket, "tx-limit").await;

    // K11: an external system limits the ongoing transaction; the CSMS is only informed.
    for _ in 0..3 {
        let reply = notify(&mut socket, "charging-limit-grid", |_| {}).await;
        assert_eq!(reply[2], corpus("charging-limit-ack")[2]);
        no_call(&mut socket).await;
    }
    let reply = notify(&mut socket, "charging-limit-evse", |_| {}).await;
    assert_eq!(reply[2], json!({}));
    let snapshot = fixture.snapshot(&client, "station-a").await;
    assert_eq!(
        grid(&snapshot, "active"),
        json!({"type":"boolean","value":true})
    );
    assert_eq!(
        grid(&snapshot, "grid_critical"),
        json!({"type":"boolean","value":true})
    );
    assert_eq!(
        grid(&snapshot, "schedules"),
        json!({"type":"unsigned_integer","value":1})
    );
    assert_eq!(
        evse(&snapshot, "active"),
        json!({"type":"boolean","value":true})
    );
    assert!(
        evse(&snapshot, "grid_critical").is_null(),
        "isGridCritical was not sent"
    );
    // An observed limit is neither a canonical command nor a native one.
    no_call(&mut socket).await;

    let forged = notify(&mut socket, "charging-limit-grid", |payload| {
        payload["chargingLimit"]["chargingLimitSource"] = json!("CSO");
    })
    .await;
    assert_eq!(
        (forged[0].clone(), forged[2].clone()),
        (json!(4), json!("PropertyConstraintViolation")),
        "K11.FR.05: a station never reports a CSO-installed limit as external"
    );
    let unconfigured = notify(&mut socket, "charging-limit-evse", |payload| {
        payload["evseId"] = json!(2);
    })
    .await;
    assert_eq!(
        (unconfigured[0].clone(), unconfigured[2].clone()),
        (json!(4), json!("ProtocolError"))
    );
    assert_eq!(fixture.snapshot(&client, "station-a").await, snapshot);

    // K13: the grid limit is released; the EVSE limit from another source stays active.
    let reply = notify(&mut socket, "cleared-charging-limit-grid", |_| {}).await;
    assert_eq!(reply[2], corpus("cleared-charging-limit-ack")[2]);
    let snapshot = fixture.snapshot(&client, "station-a").await;
    assert_eq!(
        grid(&snapshot, "active"),
        json!({"type":"boolean","value":false})
    );
    assert!(grid(&snapshot, "grid_critical").is_null());
    assert!(grid(&snapshot, "schedules").is_null());
    assert_eq!(
        evse(&snapshot, "active"),
        json!({"type":"boolean","value":true})
    );
    no_call(&mut socket).await;

    drop(socket);
    drop(process);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    let recovered = fixture.snapshot(&client, "station-a").await;
    assert_eq!(
        grid(&recovered, "active"),
        json!({"type":"boolean","value":false})
    );
    assert_eq!(
        evse(&recovered, "active"),
        json!({"type":"boolean","value":true})
    );
    let mut socket = connect(&fixture, &client, "station-a").await;
    no_call(&mut socket).await;
    let reply = notify(&mut socket, "cleared-charging-limit-evse", |_| {}).await;
    assert_eq!(reply[2], json!({}));
    let snapshot = fixture.snapshot(&client, "station-a").await;
    assert_eq!(
        evse(&snapshot, "active"),
        json!({"type":"boolean","value":false})
    );
}
