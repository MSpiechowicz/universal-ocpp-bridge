use super::support::*;
use serde_json::{Value, json};

#[tokio::test]
async fn schema_invalid_events_are_refused_with_a_property_violation_and_write_nothing() {
    let (fixture, _process) = launch(Options::default()).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;

    let corpus = wire("security-negative-cases.json");
    let cases: Vec<&Value> = corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| {
            case["level"] == "schema"
                && case["wire"][0] == 2
                && case["wire"][2] == "SecurityEventNotification"
        })
        .collect();
    assert!(
        cases.len() >= 10,
        "the pinned schema-level cases are present"
    );

    for case in cases {
        let frame = case["wire"].clone();
        let reply = exchange(&mut socket, frame.clone()).await;
        assert_eq!(reply[0], 4, "{}", case["id"]);
        assert_eq!(reply[1], frame[1], "{}", case["id"]);
        assert_eq!(reply[2], "PropertyConstraintViolation", "{}", case["id"]);
        // The refusal never echoes what the charger wrote.
        assert!(!reply.to_string().contains("yesterday evening"));
    }
    assert_eq!(stored(&client, &fixture).await, 0);

    // The session survives every refusal and still accepts a valid event.
    notify(&mut socket, wire("security-event-minimal.json")).await;
    assert_eq!(stored(&client, &fixture).await, 1);
    let heartbeat = station_call(&mut socket, "beat", "Heartbeat", json!({})).await;
    assert!(heartbeat["currentTime"].is_string());
}

#[tokio::test]
async fn an_unregistered_station_gets_a_protocol_error_and_nothing_is_written() {
    let (fixture, _process) = launch(Options::default()).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;

    let early = wire("security-event-reset-or-reboot.json");
    let reply = exchange(&mut socket, early.clone()).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[1], early[1]);
    assert_eq!(reply[2], "ProtocolError");
    assert_eq!(stored(&client, &fixture).await, 0);

    // Once the station is accepted the same event is journaled.
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    let mut accepted = early;
    accepted[1] = json!("after-boot");
    notify(&mut socket, accepted).await;
    assert_eq!(stored(&client, &fixture).await, 1);
}

#[tokio::test]
async fn an_ocpp201_station_has_no_security_event_notification() {
    let (fixture, _process) = launch_201().await;
    let client = client();
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    let booted = station_call(
        &mut socket,
        "boot",
        "BootNotification",
        json!({"chargingStation":{"model":"model","vendorName":"vendor"},"reason":"PowerUp"}),
    )
    .await;
    assert_eq!(booted["status"], "Accepted");
    fixture.connected(&client, "station-a").await;

    let reply = exchange(&mut socket, wire("security-event-minimal.json")).await;
    assert_eq!(reply[0], 4);
    assert_eq!(reply[2], "NotImplemented");
    assert_eq!(stored(&client, &fixture).await, 0);
}
