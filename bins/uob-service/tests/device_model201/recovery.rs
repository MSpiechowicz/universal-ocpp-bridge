use super::support::*;
use serde_json::json;

#[tokio::test]
async fn crash_after_native_acceptance_interrupts_report_without_replay_on_reconnect() {
    let fixture = fixture();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut station).await;
    let body = command("restart-report", "GetReport", json!({"requestId":0}));
    let request = begin(&client, &fixture, body.clone());
    let call = receive(&mut station).await;
    assert_eq!(call[2], "GetReport");
    send(&mut station, json!([3,call[1],{"status":"Accepted"}])).await;
    assert_eq!(
        completed(request).await["device_model_201"]["report"]["state"],
        "pending"
    );
    drop(child);
    drop(station);
    let mut restarted = fixture.start();
    fixture.ready(&mut restarted).await;
    let recovered = result(&client, &fixture, "restart-report").await;
    assert_eq!(recovered["lifecycle"]["stage"], "protocol_response");
    assert_eq!(recovered["device_model_201"]["native_ack"], "Accepted");
    assert_eq!(
        recovered["device_model_201"]["report"]["state"],
        "incomplete"
    );
    assert_eq!(
        recovered["device_model_201"]["report"]["reason"],
        "interrupted"
    );
    assert!(recovered["device_model_201"]["report"]["progress"].is_null());
    let mut station = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    no_call(&mut station).await;
    assert_eq!(completed(begin(&client, &fixture, body)).await, recovered);
    no_call(&mut station).await;
    boot(&mut station).await;
    let request = begin(
        &client,
        &fixture,
        command(
            "fresh-generation",
            "GetBaseReport",
            json!({"requestId":0,"reportBase":"SummaryInventory"}),
        ),
    );
    let call = receive(&mut station).await;
    assert_eq!(call[3]["requestId"], 0);
    send(&mut station, json!([3,call[1],{"status":"EmptyResultSet"}])).await;
    assert_eq!(
        completed(request).await["device_model_201"]["report"]["state"],
        "not_expected"
    );
}
