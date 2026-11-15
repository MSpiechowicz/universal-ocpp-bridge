use super::support::*;
use serde_json::json;
use std::time::Duration;

const FULL: Options = Options {
    extended: true,
    plain_trigger: false,
    log: true,
    signed: true,
};

#[tokio::test]
async fn a_redelivered_event_is_not_duplicated_after_a_daemon_restart() {
    let (fixture, process) = launch(Options::default()).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let frame = event_call(
        "first",
        "StartupOfTheDevice",
        "2026-10-07T06:16:00Z",
        Some("cold"),
    );
    notify(&mut socket, frame.clone()).await;
    assert_eq!(stored(&client, &fixture).await, 1);
    drop(socket);
    drop(process);

    let _process = restart(&fixture).await;
    let mut socket = connected(&fixture, &client).await;
    let mut redelivery = frame;
    redelivery[1] = json!("redelivered");
    notify(&mut socket, redelivery).await;
    assert_eq!(stored(&client, &fixture).await, 1);

    // The restarted daemon still stores a genuinely new event.
    notify(
        &mut socket,
        event_call(
            "next",
            "StartupOfTheDevice",
            "2026-10-07T06:17:00Z",
            Some("cold"),
        ),
    )
    .await;
    assert_eq!(stored(&client, &fixture).await, 2);
}

#[tokio::test]
async fn an_in_flight_request_is_never_resent_after_a_restart() {
    let (fixture, process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let submission = begin(
        &client,
        &fixture,
        extended_command("lost", "LogStatusNotification", None),
    );
    let call = receive(&mut socket).await;
    assert_eq!(call[2], "ExtendedTriggerMessage");
    // The daemon dies before the station answers.
    drop(process);
    let _ = submission.await;
    drop(socket);

    let _process = restart(&fixture).await;
    let mut socket = connected(&fixture, &client).await;
    no_call(&mut socket).await;
    let recovered = result(&client, &fixture, "lost").await;
    assert_eq!(recovered["lifecycle"]["stage"], "transmission_uncertain");
    assert_ne!(recovered["trigger_observation"]["status"], "observed");
}

#[tokio::test]
async fn a_compatible_message_after_a_restart_still_reconciles_inside_the_window() {
    let (fixture, process) = launch(FULL).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    let (_, pending) = answered(
        &client,
        &fixture,
        &mut socket,
        extended_command("across", "FirmwareStatusNotification", None),
        json!({"status":"Accepted"}),
    )
    .await;
    assert_eq!(pending["trigger_observation"]["status"], "pending");
    drop(socket);
    drop(process);

    let _process = restart(&fixture).await;
    let mut socket = connected(&fixture, &client).await;
    no_call(&mut socket).await;
    assert_eq!(
        station_call(
            &mut socket,
            "signed",
            "SignedFirmwareStatusNotification",
            json!({"status":"Idle"})
        )
        .await,
        json!({})
    );
    let observed = observation(
        &client,
        &fixture,
        "across",
        "observed",
        Duration::from_secs(8),
    )
    .await;
    assert_eq!(observed["schema_version"], json!({"major":1,"revision":19}));
}
