use super::{
    EVENT_SCHEMA, Error, RESULT_SCHEMA, Result, STATE_SCHEMA, Scenario, document, encoded, field,
    wire::Peer,
};
use rumqttc::Publish;
use serde_json::{Value, json};
use time::OffsetDateTime;

fn result(message: &Publish, request_id: &str, correlation: &str, station: &str) -> Result<Value> {
    if message.retain {
        return Err(Error::new("command result was retained"));
    }
    let body = document(message, &RESULT_SCHEMA)?;
    if body["schema_version"] != json!({"major":1,"revision":0})
        || body["return_route"]["request_id"] != request_id
        || body["return_route"]["origin"]["kind"] != "target"
        || body["return_route"]["origin"]["target_instance_id"] != "main"
        || body["return_route"]["origin"]["principal_id"] != "mqtt-target:main"
        || body["correlation_id"] != correlation
        || body["resource"]["station_id"] != station
    {
        return Err(Error::new("uncorrelated command result"));
    }
    Ok(body)
}

fn outcome_context(lifecycle: &Value) -> (&str, &str) {
    (
        lifecycle["stage"].as_str().unwrap_or("unknown"),
        lifecycle["error"]["code"].as_str().unwrap_or("none"),
    )
}

struct Command<'a> {
    request_id: &'a str,
    correlation: &'a str,
    resource: &'a Value,
    operation: Value,
    expires_at: &'a str,
    retain: bool,
}

async fn send(peer: &mut Peer, base: &str, case: &Scenario, request: Command<'_>) -> Result<Value> {
    let Command {
        request_id,
        correlation,
        resource,
        operation,
        expires_at,
        retain,
    } = request;
    let command = json!({
        "schema_version":{"major":1,"revision":0},
        "request_id":request_id, "correlation_id":correlation,
        "resource":resource, "operation":operation, "expires_at":expires_at
    });
    let command_topic = format!(
        "{base}/commands/{}/{}",
        encoded(&case.station),
        encoded(request_id)
    );
    let result_topic = format!(
        "{base}/results/{}/{}",
        encoded(&case.station),
        encoded(request_id)
    );
    peer.publish(
        command_topic,
        &serde_json::to_vec(&command).map_err(|_| Error::new("command encoding failed"))?,
        retain,
    )
    .await?;
    let publication = peer.wait_for(&result_topic, |_| true).await?;
    result(&publication, request_id, correlation, &case.station)
}

pub(super) fn transaction<'a>(
    snapshot: &'a Value,
    id: Option<&str>,
    ended: bool,
) -> Option<&'a Value> {
    snapshot["transactions"]
        .as_array()?
        .iter()
        .find(|transaction| {
            (id.is_none() || transaction["transaction_id"].as_str() == id)
                && (transaction["state"] == if ended { "ended" } else { "pending" }
                    || (!ended && transaction["state"] == "active"))
        })
}

#[derive(Default, serde::Serialize)]
pub struct CommandEvidence {
    // PUBACKs for command publications, excluding retained-state cleanup.
    pub broker_acknowledgements: usize,
    pub correlated_results: usize,
    pub observed_effects: usize,
    pub expired_rejected: bool,
    pub retained_replay_rejected: bool,
    pub deduplicated: bool,
    pub event_ids: Vec<String>,
    #[serde(skip)]
    pub transaction_id: Option<String>,
}

struct CommandContext<'a> {
    base: &'a str,
    case: &'a Scenario,
    resource: &'a Value,
    prefix: String,
    expiry: String,
}

impl CommandContext<'_> {
    fn start_operation(&self) -> Value {
        json!({"kind":"start","parameters":{"authorization_reference":self.case.authorization_reference}})
    }
}

pub async fn commands(
    peer: &mut Peer,
    base: &str,
    case: &Scenario,
    snapshot: &Value,
) -> Result<CommandEvidence> {
    let resource = snapshot["resources"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or(Error::new("missing command resource"))?["resource"]
        .clone();
    let baseline_ids: Vec<&str> = snapshot["transactions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|transaction| transaction["transaction_id"].as_str())
        .collect();
    let nonce = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let expiry = (OffsetDateTime::now_utc() + time::Duration::minutes(5))
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| Error::new("expiry encoding failed"))?;
    let context = CommandContext {
        base,
        case,
        resource: &resource,
        prefix: format!("ems-{}-{nonce}-{}", std::process::id(), case.station),
        expiry,
    };
    let mut evidence = CommandEvidence::default();
    reject_expired(peer, &context).await?;
    evidence.expired_rejected = true;
    evidence.broker_acknowledgements += 1;
    evidence.correlated_results += 1;
    for kind in ["start", "stop"] {
        let id = command_effect(peer, &context, kind, &baseline_ids, &mut evidence).await?;
        evidence.transaction_id = Some(id);
    }
    reject_retained_replay(peer, &context).await?;
    evidence.retained_replay_rejected = true;
    evidence.broker_acknowledgements += 1;
    evidence.correlated_results += 2;
    Ok(evidence)
}

async fn reject_expired(peer: &mut Peer, context: &CommandContext<'_>) -> Result<()> {
    let id = format!("{}-expired", context.prefix);
    let response = send(
        peer,
        context.base,
        context.case,
        Command {
            request_id: &id,
            correlation: &id,
            resource: context.resource,
            operation: context.start_operation(),
            expires_at: "2000-01-01T00:00:00Z",
            retain: false,
        },
    )
    .await?;
    if response["lifecycle"]["stage"] != "rejected"
        || response["lifecycle"]["error"]["code"] != "expired"
    {
        return Err(Error::new("expired command was not rejected"));
    }
    Ok(())
}

async fn command_effect(
    peer: &mut Peer,
    context: &CommandContext<'_>,
    kind: &str,
    baseline_ids: &[&str],
    evidence: &mut CommandEvidence,
) -> Result<String> {
    let request_id = format!("{}-{kind}", context.prefix);
    let operation = if kind == "start" {
        context.start_operation()
    } else {
        json!({"kind":"stop","parameters":{"transaction_id":evidence.transaction_id.as_deref()
            .ok_or(Error::new("start effect missing"))?}})
    };
    let response = send(
        peer,
        context.base,
        context.case,
        Command {
            request_id: &request_id,
            correlation: &request_id,
            resource: context.resource,
            operation: operation.clone(),
            expires_at: &context.expiry,
            retain: false,
        },
    )
    .await?;
    evidence.broker_acknowledgements += 1;
    evidence.correlated_results += 1;
    if response["lifecycle"]["stage"] != "protocol_response"
        || response["lifecycle"]["accepted"] != true
    {
        return Err(Error::new("station did not accept remote charging command"));
    }
    let id = observe_effect(
        peer,
        context,
        kind,
        baseline_ids,
        evidence.transaction_id.as_deref(),
    )
    .await?;
    evidence.observed_effects += 1;
    // Observe each event before issuing the next command, tying start then stop to
    // the newly identified physical transaction rather than an earlier journal entry.
    let event_id = observe_event(peer, context, kind, &id, &evidence.event_ids).await?;
    evidence.event_ids.push(event_id);
    // A retry must report the same accepted protocol decision; the host separately
    // counts physical start/stop effects.
    let duplicate = send(
        peer,
        context.base,
        context.case,
        Command {
            request_id: &request_id,
            correlation: &request_id,
            resource: context.resource,
            operation,
            expires_at: &context.expiry,
            retain: false,
        },
    )
    .await?;
    evidence.broker_acknowledgements += 1;
    evidence.correlated_results += 1;
    let first = &response["lifecycle"];
    let retry = &duplicate["lifecycle"];
    evidence.deduplicated = duplicate["resource"] == response["resource"]
        && duplicate["return_route"] == response["return_route"]
        && retry["stage"] == "protocol_response"
        && retry["accepted"] == true
        && retry["error"] == first["error"];
    if !evidence.deduplicated {
        let (first_stage, first_code) = outcome_context(first);
        let (retry_stage, retry_code) = outcome_context(retry);
        return Err(Error::owned(format!(
            "duplicate request changed outcome ({kind}: first {first_stage}/{first_code}, retry {retry_stage}/{retry_code})"
        )));
    }
    Ok(id)
}

async fn observe_effect(
    peer: &mut Peer,
    context: &CommandContext<'_>,
    kind: &str,
    baseline_ids: &[&str],
    expected_id: Option<&str>,
) -> Result<String> {
    let topic = format!("{}/state/{}", context.base, encoded(&context.case.station));
    let snapshot = peer
        .wait_for(&topic, |message| {
            serde_json::from_slice::<Value>(&message.payload).is_ok_and(|state| {
                transaction(&state, expected_id, kind == "stop").is_some_and(|effect| {
                    kind != "start"
                        || effect["transaction_id"]
                            .as_str()
                            .is_some_and(|id| !baseline_ids.contains(&id))
                })
            })
        })
        .await?;
    // Live MQTT updates have RETAIN=false even when they replace retained state.
    let state = document(&snapshot, &STATE_SCHEMA)?;
    let effect = transaction(&state, expected_id, kind == "stop")
        .ok_or(Error::new("station effect missing"))?;
    if effect["resource"]["station_id"] != context.case.station
        || effect["resource"]["bridge_id"] != context.resource["bridge_id"]
    {
        return Err(Error::new("transaction effect belongs to another resource"));
    }
    let id = field(effect, "transaction_id")?;
    if kind == "start" && baseline_ids.contains(&id) {
        return Err(Error::new("station effect predates command"));
    }
    Ok(id.to_owned())
}

async fn observe_event(
    peer: &mut Peer,
    context: &CommandContext<'_>,
    kind: &str,
    id: &str,
    previous_ids: &[String],
) -> Result<String> {
    let prefix = format!(
        "{}/events/{}/",
        context.base,
        encoded(&context.case.station)
    );
    let event_type = if kind == "start" {
        "transaction.started"
    } else {
        "transaction.ended"
    };
    let event = peer
        .wait_prefix(&prefix, |message| {
            serde_json::from_slice::<Value>(&message.payload).is_ok_and(|value| {
                value["event_type"] == event_type && value["payload"]["transaction_id"] == id
            })
        })
        .await?;
    if event.retain {
        return Err(Error::new("domain event unexpectedly retained"));
    }
    let envelope = document(&event, &EVENT_SCHEMA)?;
    let event_id = field(&envelope, "event_id")?;
    if envelope["resource"]["station_id"] != context.case.station
        || envelope["resource"]["bridge_id"] != context.resource["bridge_id"]
        || envelope["payload"]["transaction_id"] != id
        || !event.topic.ends_with(encoded(event_id).as_bytes())
        || previous_ids.iter().any(|previous| previous == event_id)
    {
        return Err(Error::new("domain event identity mismatch"));
    }
    Ok(event_id.to_owned())
}

async fn reject_retained_replay(peer: &mut Peer, context: &CommandContext<'_>) -> Result<()> {
    // A retained expired command is safe even when its live write has RETAIN=false.
    // Reconnect causes a new subscription and proves replay rejection.
    let id = format!("{}-retained", context.prefix);
    let topic = format!(
        "{}/commands/{}/{}",
        context.base,
        encoded(&context.case.station),
        encoded(&id)
    );
    let result_topic = format!(
        "{}/results/{}/{}",
        context.base,
        encoded(&context.case.station),
        encoded(&id)
    );
    let retained = json!({"schema_version":{"major":1,"revision":0},
        "request_id":id,"correlation_id":id,
        "resource":context.resource,"operation":context.start_operation(),
        "expires_at":"2000-01-01T00:00:00Z"});
    peer.publish(
        topic.clone(),
        &serde_json::to_vec(&retained)
            .map_err(|_| Error::new("retained command encoding failed"))?,
        true,
    )
    .await?;
    let live_result = peer.wait_for(&result_topic, |_| true).await?;
    let live = result(&live_result, &id, &id, &context.case.station)?;
    if live["lifecycle"]["stage"] != "rejected" {
        return Err(Error::new("expired retained publication was admitted"));
    }
    peer.force_target_reconnect(context.base).await?;
    let replay_result = peer
        .wait_for(&result_topic, |message| {
            serde_json::from_slice::<Value>(&message.payload)
                .is_ok_and(|value| value["lifecycle"]["error"]["detail"] == "mqtt.retained_command")
        })
        .await?;
    let replay = result(&replay_result, &id, &id, &context.case.station)?;
    let rejected = replay["lifecycle"]["error"]["detail"] == "mqtt.retained_command";
    peer.publish(topic, &[], true).await?;
    if !rejected {
        return Err(Error::new("retained replay was not rejected"));
    }
    Ok(())
}
