use super::support::*;
use serde_json::{Value, json};

#[tokio::test]
async fn normalizes_and_persists_native_schedule() {
    let fixture = Fixture::new();
    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    let client = client();
    let mut station = fixture.station("station-a").await;
    boot(&mut station).await;
    let snapshot = fixture.snapshot(&client, "station-a").await;
    let mut stored = Vec::new();

    for (id, connector, requested_unit, reply, expected) in valid_cases() {
        let value = query(
            &client,
            &fixture,
            &mut station,
            command(id, connector, requested_unit),
            reply,
        )
        .await;
        assert_eq!(value["schema_version"], json!({"major":1,"revision":4}));
        assert_eq!(value["composite_schedule_16"], expected, "{id}");
        assert_eq!(value["lifecycle"]["stage"], "protocol_response");
        assert_eq!(
            value["lifecycle"]["accepted"],
            expected["status"] == "Accepted"
        );
        if expected["status"] == "Rejected" {
            assert_eq!(value["lifecycle"]["error"]["code"], "protocol_rejected");
        } else {
            assert!(value["lifecycle"].get("error").is_none());
        }
        no_invented_effects(&value);
        assert_eq!(fixture.snapshot(&client, "station-a").await, snapshot);
        stored.push((id, value));
    }
    verify_hostile_replies(&client, &fixture, &mut station).await;
    drop(station);
    drop(child);

    let mut child = fixture.start();
    fixture.ready(&mut child).await;
    for (id, expected) in stored {
        assert_eq!(result(&client, &fixture, id).await, expected);
    }
    let mut station = fixture.station("station-a").await;
    no_call(&mut station).await;
}

type NativeCase = (&'static str, u32, Option<&'static str>, Value, Value);

#[allow(clippy::too_many_lines)] // Independent native fixtures and their complete public evidence.
fn valid_cases() -> Vec<NativeCase> {
    let detailed: Value = serde_json::from_str(
        r#"{"status":"Accepted","connectorId":1,"scheduleStart":"2026-10-01T12:00:00+02:00",
        "chargingSchedule":{"duration":90,"startSchedule":"2026-10-01T09:59:00Z",
        "chargingRateUnit":"A","minChargingRate":4.0,"chargingSchedulePeriod":[
        {"startPeriod":0,"limit":0},{"startPeriod":30,"limit":900719925474099.1,"numberPhases":4},
        {"startPeriod":60,"limit":1.20,"numberPhases":1}]}}"#,
    )
    .unwrap();
    let detailed_expected = json!({
        "request":{"connector_id":1,"duration":120,"charging_rate_unit":"A"},
        "status":"Accepted","connector_id":1,"schedule_start":"2026-10-01T10:00:00Z",
        "charging_schedule":{"duration":90,"start_schedule":"2026-10-01T09:59:00Z",
            "charging_rate_unit":"A","min_charging_rate":"4","charging_schedule_period":[
                {"start_period":0,"limit":"0"},
                {"start_period":30,"limit":"900719925474099.1","number_phases":4},
                {"start_period":60,"limit":"1.2","number_phases":1}
            ]}
    });
    let mut rejected = detailed.clone();
    rejected["status"] = json!("Rejected");
    let mut rejected_expected = detailed_expected.clone();
    rejected_expected["status"] = json!("Rejected");
    let mut schedule_only = detailed.clone();
    schedule_only["status"] = json!("Rejected");
    schedule_only
        .as_object_mut()
        .unwrap()
        .remove("scheduleStart");
    schedule_only.as_object_mut().unwrap().remove("connectorId");
    let mut schedule_only_expected = detailed_expected.clone();
    schedule_only_expected["status"] = json!("Rejected");
    schedule_only_expected
        .as_object_mut()
        .unwrap()
        .remove("schedule_start");
    schedule_only_expected
        .as_object_mut()
        .unwrap()
        .remove("connector_id");
    let mut watts = accepted("W");
    watts["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"] =
        serde_json::from_str("4500.5").unwrap();
    let mut cases = vec![
        (
            "grid-zero",
            0,
            None,
            accepted("W"),
            evidence(0, None, "W", "0"),
        ),
        ("connector-a", 1, Some("A"), detailed, detailed_expected),
        (
            "connector-w",
            2,
            Some("W"),
            watts,
            evidence(2, Some("W"), "W", "4500.5"),
        ),
        (
            "native-choice-a",
            2,
            None,
            accepted("A"),
            evidence(2, None, "A", "0"),
        ),
        (
            "rejected-empty",
            0,
            None,
            json!({"status":"Rejected"}),
            json!({
                "request":{"connector_id":0,"duration":120},"status":"Rejected"
            }),
        ),
        (
            "rejected-metadata",
            1,
            Some("A"),
            rejected,
            rejected_expected,
        ),
        (
            "rejected-schedule-only",
            1,
            Some("A"),
            schedule_only,
            schedule_only_expected,
        ),
        (
            "rejected-anchor",
            2,
            None,
            json!({
                "status":"Rejected","connectorId":2,"scheduleStart":"2026-10-01T12:00:00+02:00"
            }),
            json!({
                "request":{"connector_id":2,"duration":120},"status":"Rejected",
                "connector_id":2,"schedule_start":"2026-10-01T10:00:00Z"
            }),
        ),
    ];
    for (id, lexeme, exact) in [
        ("exponent", "9.007199254740991e14", "900719925474099.1"),
        ("trailing-zero", "12.30000", "12.3"),
        (
            "native-decimal-max",
            "79228162514264337593543950335",
            "79228162514264337593543950335",
        ),
    ] {
        let reply = serde_json::from_str(&format!(
            r#"{{"status":"Accepted","scheduleStart":"2026-10-01T10:00:00Z",
            "chargingSchedule":{{"chargingRateUnit":"W","chargingSchedulePeriod":[
            {{"startPeriod":0,"limit":{lexeme}}}]}}}}"#
        ))
        .unwrap();
        cases.push((id, 0, None, reply, evidence(0, None, "W", exact)));
    }
    cases
}

fn evidence(connector: u32, forced: Option<&str>, unit: &str, rate: &str) -> Value {
    let mut request = json!({"connector_id":connector,"duration":120});
    if let Some(forced) = forced {
        request["charging_rate_unit"] = json!(forced);
    }
    json!({
        "request":request,"status":"Accepted","schedule_start":"2026-10-01T10:00:00Z",
        "charging_schedule":{"charging_rate_unit":unit,
            "charging_schedule_period":[{"start_period":0,"limit":rate}]}
    })
}

fn no_invented_effects(value: &Value) {
    for field in ["observed_effects", "configuration_observations"] {
        assert!(value.get(field).is_none_or(|entry| entry == &json!([])));
    }
    for field in [
        "configuration",
        "trigger_observation",
        "trigger_observation_201",
    ] {
        assert!(value.get(field).is_none());
    }
}

#[allow(clippy::too_many_lines)] // Each hostile reply must cross the actual process/socket boundary.
async fn verify_hostile_replies(client: &reqwest::Client, fixture: &Fixture, station: &mut Socket) {
    let mut replies = vec![
        json!({"status":"Accepted"}),
        json!({"status":"Unsupported"}),
        json!({"status":"Rejected","connectorId":null}),
        json!({"status":"Rejected","unknown":true}),
    ];
    let mut rejected_wrong_unit = accepted("W");
    rejected_wrong_unit["status"] = json!("Rejected");
    replies.push(rejected_wrong_unit);
    let mut missing_anchor = accepted("A");
    missing_anchor
        .as_object_mut()
        .unwrap()
        .remove("scheduleStart");
    replies.push(missing_anchor);
    for (pointer, value) in [
        ("/scheduleStart", json!(null)),
        ("/scheduleStart", json!("invalid")),
        ("/scheduleStart", json!("9999-12-31T23:59:59Z")),
        ("/chargingSchedule", json!(null)),
        ("/connectorId", json!(2)),
        ("/chargingSchedule/duration", json!(0)),
        ("/chargingSchedule/duration", json!(121)),
        ("/chargingSchedule/duration", json!(null)),
        ("/chargingSchedule/duration", json!(1.5)),
        ("/chargingSchedule/startSchedule", json!("invalid")),
        ("/chargingSchedule/startSchedule", json!(null)),
        ("/chargingSchedule/chargingRateUnit", json!("W")),
        ("/chargingSchedule/unknown", json!(true)),
        ("/chargingSchedule/chargingSchedulePeriod", json!([])),
        ("/chargingSchedule/chargingSchedulePeriod", json!(null)),
        ("/chargingSchedule/chargingSchedulePeriod", json!([null])),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":-1,"limit":0}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0.5,"limit":0}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":0,"limit":0,"unknown":true}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([{"startPeriod":1,"limit":0}]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([
                {"startPeriod":0,"limit":0},{"startPeriod":0,"limit":1}
            ]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([
                {"startPeriod":0,"limit":0},{"startPeriod":30,"limit":1},{"startPeriod":20,"limit":1}
            ]),
        ),
        (
            "/chargingSchedule/chargingSchedulePeriod",
            json!([
                {"startPeriod":0,"limit":0},{"startPeriod":120,"limit":1}
            ]),
        ),
        ("/chargingSchedule/minChargingRate", json!(-1)),
        ("/chargingSchedule/minChargingRate", json!(0.01)),
        ("/chargingSchedule/minChargingRate", json!(null)),
        ("/chargingSchedule/minChargingRate", json!("1")),
    ] {
        let mut reply = accepted("A");
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        reply
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.to_owned(), value);
        replies.push(reply);
    }
    for phases in [
        json!(0),
        json!(-1),
        json!(null),
        json!(1.5),
        json!(2_147_483_648_u64),
    ] {
        let mut reply = accepted("A");
        reply["chargingSchedule"]["chargingSchedulePeriod"][0]["numberPhases"] = phases;
        replies.push(reply);
    }
    for lexeme in [
        "-1",
        "0.01",
        "\"1\"",
        "null",
        "true",
        "79228162514264337593543950336",
        "7922816251426433759354395033.6",
        "1.00000000000000000000000000001",
        "1e-29",
        "1e29",
    ] {
        let mut reply = accepted("A");
        reply["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"] =
            serde_json::from_str(lexeme).unwrap();
        replies.push(reply);
    }
    for (index, reply) in replies.into_iter().enumerate() {
        let value = query(
            client,
            fixture,
            station,
            command(&format!("hostile-{index}"), 1, Some("A")),
            reply,
        )
        .await;
        assert_eq!(
            value["lifecycle"]["stage"], "transmission_uncertain",
            "hostile-{index}"
        );
        assert!(value.get("composite_schedule_16").is_none());
        no_invented_effects(&value);
    }
    let request = begin(client, fixture, command("native-callerror", 0, None));
    let call = receive(station).await;
    assert_eq!(call[2], "GetCompositeSchedule");
    send(
        station,
        json!([4, call[1], "NotSupported", "untrusted native detail", {}]),
    )
    .await;
    let value = completed(request).await;
    assert_eq!(value["lifecycle"]["stage"], "protocol_response");
    assert_eq!(value["lifecycle"]["accepted"], false);
    assert_eq!(value["lifecycle"]["error"]["code"], "protocol_rejected");
    assert!(value.get("composite_schedule_16").is_none());
    assert!(!value.to_string().contains("untrusted native detail"));
    assert_eq!(result(client, fixture, "native-callerror").await, value);
}
