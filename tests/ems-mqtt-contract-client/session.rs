use super::{
    CatalogEvidence, Demo, Error, Evidence, POINT_SCHEMA, Result, STATE_SCHEMA, Scenario,
    ScenarioEvidence, VALUE_SCHEMA, current, document, encoded, exercise, wire,
};
use serde_json::{Value, json};
use std::{future::Future, path::Path, time::Duration};

pub(super) struct Connection<'a> {
    pub broker_url: &'a str,
    pub ca_file: &'a Path,
    pub username: &'a str,
    pub password_file: &'a Path,
}

pub(super) async fn run_session<F: Future<Output = ()>>(
    connection: Connection<'_>,
    demo: &Demo,
    exercise: bool,
    allow_remote_exercise: bool,
    outage: F,
    expect_outage: bool,
) -> Result<Evidence> {
    if demo.bridge_id.is_empty()
        || demo.scenario.len() != 2
        || demo.scenario[0].protocol != "ocpp16"
        || demo.scenario[1].protocol != "ocpp201"
        || demo.scenario[0].station == demo.scenario[1].station
    {
        return Err(Error::new(
            "scenario must cover distinct OCPP 1.6 and 2.0.1 stations",
        ));
    }
    tokio::time::timeout(
        Duration::from_secs(if expect_outage {
            120
        } else if exercise {
            110
        } else {
            35
        }),
        async {
            let base = format!("uob/v1/demo/{}", encoded(&demo.bridge_id));
            let mut peer = wire::Peer::connect(
                connection.broker_url,
                connection.ca_file,
                connection.username,
                connection.password_file,
                &base,
                allow_remote_exercise,
                exercise,
            )
            .await?;
            let availability = peer
                .wait_for(&format!("{base}/availability"), |publication| {
                    serde_json::from_slice::<Value>(&publication.payload)
                        .is_ok_and(|body| body["status"] == "online")
                })
                .await?;
            let status: Value = serde_json::from_slice(&availability.payload)
                .map_err(|_| Error::new("invalid availability JSON"))?;
            if status["schema_version"] != json!({"major":1,"revision":0})
                || status["environment"] != "demo"
                || status["bridge_id"] != demo.bridge_id
                || status["target_instance_id"] != "main"
            {
                return Err(Error::new("availability identity mismatch"));
            }
            let mut scenarios = Vec::new();
            let mut baselines = Vec::new();
            let mut completed = Vec::new();
            for case in &demo.scenario {
                let (evidence, baseline, id) =
                    inspect_scenario(&mut peer, &base, demo, case, exercise).await?;
                if expect_outage {
                    baselines.push(baseline);
                }
                if let Some(id) = id {
                    completed.push((case, id));
                }
                scenarios.push(evidence);
            }
            if exercise {
                check_retained_effects(&connection, &base, demo, completed, allow_remote_exercise)
                    .await?;
            }
            if expect_outage {
                let ((), reconnected) = tokio::join!(outage, peer.wait_reconnected());
                reconnected?;
                check_reconnected(&mut peer, &base, demo, &status, baselines, &mut scenarios)
                    .await?;
            }
            Ok(Evidence {
                status: "passed",
                connected: peer.connected,
                target_online: true,
                subscriptions_acknowledged: peer.subscriptions,
                consumer_reconnects: peer.reconnects,
                retained_after_reconnect: expect_outage,
                scenarios,
            })
        },
    )
    .await
    .map_err(|_| Error::new("MQTT contract probe deadline exceeded"))?
}

async fn inspect_scenario(
    peer: &mut wire::Peer,
    base: &str,
    demo: &Demo,
    case: &Scenario,
    exercise: bool,
) -> Result<(ScenarioEvidence, (Value, Value), Option<String>)> {
    let station = encoded(&case.station);
    let point = encoded(&case.point_id);
    let descriptor_topic = format!("{base}/points/{station}/{point}");
    let value_topic = format!("{base}/values/{station}/{point}");
    let state_topic = format!("{base}/state/{station}");
    let descriptor_message = peer.wait_for(&descriptor_topic, |_| true).await?;
    if !descriptor_message.retain {
        return Err(Error::new("point descriptor is not retained"));
    }
    let descriptor = document(&descriptor_message, &POINT_SCHEMA)?;
    if descriptor["point_id"] != case.point_id
        || descriptor["resource"]["bridge_id"] != demo.bridge_id
        || descriptor["resource"]["station_id"] != case.station
        || descriptor["resource"]["native_protocol_reference"]["protocol"] != case.protocol
        || descriptor["value_type"] != "decimal"
        || descriptor["unit"] != case.unit
        || descriptor["access"] != "read_only"
    {
        return Err(Error::new("canonical descriptor metadata mismatch"));
    }
    let value_message = peer.wait_for(&value_topic, |_| true).await?;
    if !value_message.retain {
        return Err(Error::new("point value is not retained"));
    }
    let value = document(&value_message, &VALUE_SCHEMA)?;
    if value["point_id"] != case.point_id
        || value["value"] != json!({"type":"decimal", "value":case.value})
        || value["observed_at"] != case.observed_at
        || value["source_time"] != json!(case.source_time)
        || value["measurement"]["original_value"] != json!(case.measurement_original_value)
        || value["measurement"]["original_unit"] != json!(case.measurement_original_unit)
        || value["quality"] != case.quality
        || value["freshness"] != case.freshness
    {
        return Err(Error::new(
            "canonical point value or exact metadata mismatch",
        ));
    }
    let snapshot_message = peer.wait_for(&state_topic, |_| true).await?;
    if !snapshot_message.retain {
        return Err(Error::new("station state is not retained"));
    }
    let snapshot = document(&snapshot_message, &STATE_SCHEMA)?;
    if snapshot["station"]["bridge_id"] != demo.bridge_id
        || snapshot["station"]["station_id"] != case.station
        || !snapshot["resources"].as_array().is_some_and(|resources| {
            resources.iter().any(|resource| {
                resource["resource"]["native_protocol_reference"]["protocol"] == case.protocol
            })
        })
    {
        return Err(Error::new("station state identity or protocol mismatch"));
    }
    let current_at_receive = current(&value)?;
    if case
        .expected_current
        .is_some_and(|expected| expected != current_at_receive)
    {
        return Err(Error::new(
            "measurement age/quality differs from expectation",
        ));
    }
    let mut commands = if exercise {
        exercise::commands(peer, base, case, &snapshot).await?
    } else {
        exercise::CommandEvidence::default()
    };
    let id = commands.transaction_id.take();
    let evidence = ScenarioEvidence {
        protocol: case.protocol.clone(),
        station: case.station.clone(),
        catalog: CatalogEvidence {
            descriptor: true,
            exact_value: true,
            retained_state: true,
        },
        current_at_receive,
        current_after_reconnect: None,
        commands,
    };
    Ok((evidence, (descriptor, value), id))
}

async fn check_retained_effects(
    connection: &Connection<'_>,
    base: &str,
    demo: &Demo,
    completed: Vec<(&Scenario, String)>,
    allow_remote_exercise: bool,
) -> Result<()> {
    // A fresh subscription must receive RETAIN=true; a live update cannot prove persistence.
    let mut delayed = wire::Peer::connect(
        connection.broker_url,
        connection.ca_file,
        connection.username,
        connection.password_file,
        base,
        allow_remote_exercise,
        false,
    )
    .await?;
    for (case, id) in completed {
        let topic = format!("{base}/state/{}", encoded(&case.station));
        let publication = delayed.wait_for(&topic, |message| message.retain).await?;
        let state = document(&publication, &STATE_SCHEMA)?;
        let effect = exercise::transaction(&state, Some(&id), true)
            .ok_or(Error::new("retained station state predates command"))?;
        if state["station"]["station_id"] != case.station
            || state["station"]["bridge_id"] != demo.bridge_id
            || effect["resource"]["station_id"] != case.station
            || effect["resource"]["bridge_id"] != demo.bridge_id
        {
            return Err(Error::new("retained station state identity mismatch"));
        }
    }
    Ok(())
}

async fn check_reconnected(
    peer: &mut wire::Peer,
    base: &str,
    demo: &Demo,
    status: &Value,
    baselines: Vec<(Value, Value)>,
    scenarios: &mut [ScenarioEvidence],
) -> Result<()> {
    let online = peer
        .wait_for(&format!("{base}/availability"), |publication| {
            serde_json::from_slice::<Value>(&publication.payload)
                .is_ok_and(|body| body["status"] == "online")
        })
        .await?;
    let recovered: Value = serde_json::from_slice(&online.payload)
        .map_err(|_| Error::new("invalid resumed availability"))?;
    if &recovered != status {
        return Err(Error::new("target availability changed across reconnect"));
    }
    for ((case, (old_descriptor, old_value)), evidence) in
        demo.scenario.iter().zip(baselines).zip(scenarios)
    {
        let station = encoded(&case.station);
        let point = encoded(&case.point_id);
        let descriptor = peer
            .wait_for(&format!("{base}/points/{station}/{point}"), |message| {
                message.retain
            })
            .await?;
        let value = peer
            .wait_for(&format!("{base}/values/{station}/{point}"), |message| {
                message.retain
            })
            .await?;
        let state = peer
            .wait_for(&format!("{base}/state/{station}"), |message| message.retain)
            .await?;
        let resumed = document(&value, &VALUE_SCHEMA)?;
        if document(&descriptor, &POINT_SCHEMA)? != old_descriptor
            || resumed != old_value
            || document(&state, &STATE_SCHEMA)?["station"]["station_id"] != case.station
        {
            return Err(Error::new(
                "retained catalog/state changed or disappeared across reconnect",
            ));
        }
        let fresh = current(&resumed)?;
        if case
            .expected_current
            .is_some_and(|expected| expected != fresh)
        {
            return Err(Error::new(
                "resumed measurement age/quality differs from expectation",
            ));
        }
        evidence.current_after_reconnect = Some(fresh);
    }
    Ok(())
}
