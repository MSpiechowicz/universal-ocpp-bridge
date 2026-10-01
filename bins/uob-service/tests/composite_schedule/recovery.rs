use super::support::*;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn delayed_reply_keeps_heartbeat_progress() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    let body = command("delayed", 1, Some("A"));
    let pending = begin(&client, &fixture, body.clone());
    let call = receive(&mut station).await;
    assert_eq!(call[2], "GetCompositeSchedule");
    assert_eq!(
        call[3],
        json!({"connectorId":1,"duration":120,"chargingRateUnit":"A"})
    );

    let heartbeat = tokio::time::timeout(
        Duration::from_secs(2),
        station_call(
            &mut station,
            "heartbeat-during-schedule",
            "Heartbeat",
            json!({}),
        ),
    )
    .await
    .expect("heartbeat completes before native query reply");
    assert!(heartbeat["currentTime"].as_str().is_some());
    assert!(!pending.is_finished());
    let dispatched = result(&client, &fixture, "delayed").await;
    assert_eq!(dispatched["lifecycle"]["stage"], "dispatched");
    assert_eq!(
        completed(begin(&client, &fixture, body.clone())).await,
        dispatched
    );
    no_call(&mut station).await;

    send(&mut station, json!([3, call[1], accepted("A")])).await;
    let complete = completed(pending).await;
    assert_eq!(complete["composite_schedule_16"]["status"], "Accepted");
    assert_eq!(
        complete["composite_schedule_16"]["charging_schedule"]["charging_schedule_period"][0]["limit"],
        "0"
    );
    assert_eq!(result(&client, &fixture, "delayed").await, complete);
    assert_eq!(completed(begin(&client, &fixture, body)).await, complete);
    no_call(&mut station).await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep old/new socket generations and persisted outcomes together.
async fn disconnect_restart_never_replays_schedule_query() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    let body = command("disconnected-query", 2, None);
    let pending = begin(&client, &fixture, body.clone());
    let old_call = receive(&mut station).await;
    assert_eq!(old_call[2], "GetCompositeSchedule");
    assert_eq!(old_call[3], json!({"connectorId":2,"duration":120}));
    assert_eq!(
        completed(begin(&client, &fixture, body.clone())).await["lifecycle"]["stage"],
        "dispatched"
    );
    no_call(&mut station).await;
    drop(station);
    let uncertain = completed(pending).await;
    assert_uncertain(&uncertain);
    assert_eq!(
        result(&client, &fixture, "disconnected-query").await,
        uncertain
    );
    wait_disconnected(&client, &fixture).await;

    let mut station = fixture.station("station-a").await;
    no_call(&mut station).await;
    fixture.connected(&client, "station-a").await;
    assert_eq!(
        completed(begin(&client, &fixture, body.clone())).await,
        uncertain
    );
    no_call(&mut station).await;
    let denied = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&command("unregistered-generation", 2, None))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 400);
    assert_eq!(
        denied.json::<Value>().await.unwrap()["lifecycle"]["error"]["code"],
        "policy_rejected"
    );
    no_call(&mut station).await;
    boot(&mut station).await;

    let fresh = begin(
        &client,
        &fixture,
        command("explicit-after-reconnect", 2, Some("W")),
    );
    let fresh_call = receive(&mut station).await;
    assert_eq!(fresh_call[2], "GetCompositeSchedule");
    assert_eq!(
        fresh_call[3],
        json!({"connectorId":2,"duration":120,"chargingRateUnit":"W"})
    );
    assert_ne!(fresh_call[1], old_call[1]);
    // A valid old reply on this new socket cannot attach to either the old terminal result
    // or the independent pending request. The heartbeat establishes reader progress.
    send(&mut station, json!([3, old_call[1], accepted("W")])).await;
    station_call(&mut station, "stale-reply-barrier", "Heartbeat", json!({})).await;
    assert!(!fresh.is_finished());
    assert_eq!(
        result(&client, &fixture, "disconnected-query").await,
        uncertain
    );
    send(&mut station, json!([3, fresh_call[1], accepted("W")])).await;
    let fresh_result = completed(fresh).await;
    assert_eq!(
        fresh_result["composite_schedule_16"]["request"],
        json!({
            "connector_id":2,"duration":120,"charging_rate_unit":"W"
        })
    );
    assert_eq!(fresh_result["composite_schedule_16"]["status"], "Accepted");

    let timed_out = verify_timeout_and_late_reply(&client, &fixture, &mut station).await;
    drop(station);
    drop(child);
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    assert_eq!(
        result(&client, &fixture, "disconnected-query").await,
        uncertain
    );
    assert_eq!(
        result(&client, &fixture, "explicit-after-reconnect").await,
        fresh_result
    );
    assert_eq!(
        result(&client, &fixture, "timed-out-query").await,
        timed_out
    );
    let mut station = fixture.station("station-a").await;
    no_call(&mut station).await;
    boot(&mut station).await;
    assert_eq!(
        completed(begin(&client, &fixture, body.clone())).await,
        uncertain
    );
    no_call(&mut station).await;
    let mut conflict = body;
    conflict["operation"]["parameters"]["payload"]["duration"] = json!(60);
    let conflict = begin(&client, &fixture, conflict).await.unwrap();
    assert_eq!(conflict.status(), 409);
    no_call(&mut station).await;
    let new_result = query(
        &client,
        &fixture,
        &mut station,
        command("explicit-after-restart", 0, None),
        accepted("A"),
    )
    .await;
    assert_eq!(
        new_result["composite_schedule_16"]["request"],
        json!({"connector_id":0,"duration":120})
    );
    assert_eq!(new_result["composite_schedule_16"]["status"], "Accepted");
    assert_eq!(
        result(&client, &fixture, "disconnected-query").await,
        uncertain
    );
    assert_eq!(
        result(&client, &fixture, "explicit-after-reconnect").await,
        fresh_result
    );
}

async fn wait_disconnected(client: &reqwest::Client, fixture: &Fixture) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if fixture.snapshot(client, "station-a").await["connectivity"]["status"]
                == "disconnected"
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("previous socket generation finishes cleanup");
}

fn assert_uncertain(value: &Value) {
    assert_eq!(value["lifecycle"]["stage"], "transmission_uncertain");
    assert!(value.get("composite_schedule_16").is_none());
    assert!(
        value
            .get("observed_effects")
            .is_none_or(|effects| effects == &json!([]))
    );
}

async fn verify_timeout_and_late_reply(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
) -> Value {
    let body = command("timed-out-query", 0, None);
    let pending = begin(client, fixture, body.clone());
    let call = receive(socket).await;
    assert_eq!(call[2], "GetCompositeSchedule");
    assert_eq!(call[3], json!({"connectorId":0,"duration":120}));
    let value = tokio::time::timeout(Duration::from_secs(40), completed(pending))
        .await
        .expect("native 30-second response deadline terminates query");
    assert_uncertain(&value);
    send(socket, json!([3, call[1], accepted("A")])).await;
    station_call(socket, "late-timeout-barrier", "Heartbeat", json!({})).await;
    assert_eq!(result(client, fixture, "timed-out-query").await, value);
    assert_eq!(completed(begin(client, fixture, body)).await, value);
    no_call(socket).await;
    value
}
