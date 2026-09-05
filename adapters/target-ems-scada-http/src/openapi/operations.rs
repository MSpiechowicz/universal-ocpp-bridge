use super::schemas::reference;
use crate::error::IntegrationErrorCode as Error;
use schemars::JsonSchema;
use serde_json::{Map, Value, json};

fn local(name: &str) -> Value {
    json!({"$ref":format!("#/components/schemas/{name}")})
}

pub(super) fn path(name: &str) -> Value {
    let (method, success, schema, description) = match name {
        "capabilities" => (
            "get",
            "200",
            local("CapabilityDocument"),
            "Discover runtime bounds and the caller's permissions.",
        ),
        "openapi" => (
            "get",
            "200",
            json!({"type":"object"}),
            "Read the versioned OpenAPI document.",
        ),
        "schemas" => (
            "get",
            "200",
            json!({"type":"object"}),
            "Read an unmodified canonical Draft 2020-12 schema.",
        ),
        "stations" => (
            "get",
            "200",
            local("StationPage"),
            "Read a bounded, scoped inventory page. Empty pages can have a next_cursor.",
        ),
        "station" => (
            "get",
            "200",
            reference("station-snapshot"),
            "Read a current station snapshot, including native OCPP references.",
        ),
        "points" => (
            "get",
            "200",
            local("PointPage"),
            "Read bounded point descriptions and values. EVSE/connector filters require station_id. bridge_id resolves a named station; without station_id the caller's granted bridges are scanned.",
        ),
        "point" => (
            "get",
            "200",
            local("PointView"),
            "Read one point within a required station and optional EVSE/connector.",
        ),
        "commands" => (
            "post",
            "202",
            local("AcceptedCommand"),
            "Durably admit or replay an idempotent command. Admission is separate from protocol response and observed effect.",
        ),
        "command_status" => (
            "get",
            "200",
            reference("command-result"),
            "Read a scoped command result belonging to this target instance.",
        ),
        "events" => (
            "get",
            "200",
            json!({"type":"string"}),
            "Subscribe to durable SSE records; see event payload schemas and recovery semantics.",
        ),
        _ => panic!("undocumented integration resource {name}"),
    };
    let mut responses = errors(name);
    let media = match name {
        "events" => "text/event-stream",
        "schemas" => "application/schema+json",
        _ => "application/json",
    };
    responses.insert(
        success.into(),
        json!({"description":description, "content":{media:{"schema":schema}}}),
    );
    let mut operation = json!({"operationId":name, "summary":description, "responses":responses,
        "parameters":parameters(name)});
    if matches!(name, "capabilities" | "openapi" | "schemas") {
        operation["description"] = json!(
            "Anonymous access only when no credential file is configured on loopback. Otherwise integrationBearer is required."
        );
        operation["security"] = json!([{"integrationBearer":[]},{}]);
    }
    if name == "commands" {
        operation["requestBody"] = json!({"required":true,"content":{"application/json":{
            "schema":local("CommandRequest"), "examples":{"start":{"value":command_example()}}
        }}});
        operation["description"] = json!(
            "Only control operations supported by the resource are admitted. The schema preserves canonical operation variants; privileged kind=ocpp requests return 403. request_id is at most 256 bytes and cannot be '.' or '..'. Unknown fields, origin injection and unknown operation variants are rejected. Body bytes and admission concurrency are bounded by capabilities."
        );
    }
    if name == "events" {
        operation["x-uob-sse-events"] = json!({
            "durable":{"schema":reference("event-envelope"),"id":"opaque durable cursor"},
            "gap":{"schema":local("CursorGap"),"terminal":true},
            "error":{"schema":local("IntegrationError"),"terminal":true}
        });
        operation["responses"]["410"] = json!({"description":"Durable cursor expired; fetch a fresh snapshot and resubscribe without it.",
            "content":{"application/json":{"schema":local("CursorGap")}}});
    }
    with_head(method, name, &operation)
}

fn with_head(method: &str, name: &str, operation: &Value) -> Value {
    let mut result = json!({method:operation});
    if method == "get" {
        let mut head = result["get"].clone();
        head["operationId"] = json!(format!("{name}_head"));
        head["summary"] = json!("Same handler as GET; response body omitted.");
        head.as_object_mut().unwrap().remove("x-uob-sse-events");
        for response in head["responses"].as_object_mut().unwrap().values_mut() {
            response.as_object_mut().unwrap().remove("content");
        }
        result["head"] = head;
    }
    result
}

fn errors(name: &str) -> Map<String, Value> {
    let mut grouped: std::collections::BTreeMap<u16, Vec<&str>> = std::collections::BTreeMap::new();
    for &error in Error::ALL {
        // Include common transport errors and the shared port errors on state-bearing routes.
        let common = matches!(
            error,
            Error::Unauthenticated
                | Error::InvalidCredential
                | Error::CapacityExhausted
                | Error::UnsupportedOperation
        );
        let state = !matches!(name, "capabilities" | "openapi" | "schemas");
        let command_only = matches!(
            error,
            Error::RequestConflict
                | Error::CommandPolicyRejected
                | Error::CommandUnsupported
                | Error::CommandBusy
                | Error::PayloadTooLarge
        );
        if common
            || (state && (!command_only || name == "commands"))
            || (name == "schemas"
                && matches!(error, Error::UnknownResource | Error::InvalidRequest))
        {
            grouped
                .entry(error.status().as_u16())
                .or_default()
                .push(error.as_str());
        }
    }
    grouped.into_iter().map(|(status,codes)| {
        let schema = if name == "commands" && matches!(status,400|403|409|410|422) {
            json!({"oneOf":[local("IntegrationError"),reference("command-result")]})
        } else { local("IntegrationError") };
        let mut response = json!({"description":codes.join(", "),"content":{"application/json":{"schema":schema}},"x-uob-error-codes":codes});
        if status==401 { response["headers"] = json!({"WWW-Authenticate":{"schema":{"type":"string","const":"Bearer"},"description":"Bearer challenge"}}); }
        (status.to_string(),response)
    }).collect()
}

fn parameters(name: &str) -> Vec<Value> {
    let mut parameters = match name {
        "stations" => query::<crate::request::PageParameters>(),
        "station" | "point" => query::<crate::request::ResourceParameters>(),
        "points" => query::<crate::points::PointPageParameters>(),
        "events" => query::<crate::events::request::Parameters>(),
        _ => vec![],
    };
    let path = match name {
        "station" => Some("station_id"),
        "point" => Some("point_id"),
        "command_status" => Some("request_id"),
        "schemas" => Some("schema"),
        _ => None,
    };
    if let Some(path) = path {
        let schema = if path == "schema" {
            json!({"type":"string","enum":super::schemas::CANONICAL.iter().map(|(file,_)|*file).collect::<Vec<_>>()})
        } else {
            json!({"type":"string","minLength":1})
        };
        parameters.push(json!({"name":path,"in":"path","required":true,"schema":schema}));
    }
    for parameter in &mut parameters {
        let field = parameter["name"].as_str().unwrap().to_owned();
        if parameter["in"] == "query" {
            let description = match field.as_str() {
                "limit" => "Page size; reject zero or above maximum_page_size.",
                "after" => {
                    "Opaque position from the same endpoint and filters. Never interchange list and durable cursors."
                }
                "bridge_id" => {
                    "Required when the credential spans multiple bridges for an explicitly addressed station."
                }
                "station_id" if name == "station" => {
                    "Accepted for compatibility; the path station_id is authoritative."
                }
                "station_id" => {
                    "Owning station identity; required for an individual point and SSE."
                }
                "evse_id" | "connector_id" if name == "station" => {
                    "Not valid for station snapshots: supplying this filter returns 400."
                }
                "types" => {
                    "Comma-separated exact event types: at most 8 entries of 1–128 bytes, at most 1031 bytes total."
                }
                _ => "Optional canonical resource filter below the named station.",
            };
            parameter["description"] = json!(description);
            if field == "station_id" && matches!(name, "point" | "events") {
                parameter["required"] = json!(true);
            }
        }
    }
    if name == "events" {
        parameters.push(json!({"name":"Last-Event-ID","in":"header","required":false,
            "description":"One durable uob:event: cursor, at most 512 bytes without control characters. Must agree with after if both are supplied.",
            "schema":{"type":"string","maxLength":512}}));
    }
    parameters
}

fn query<T: JsonSchema>() -> Vec<Value> {
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    schema["properties"].as_object().unwrap().iter().map(|(name,_)| {
        let schema = if name=="limit" { json!({"type":"integer","minimum":1,"maximum":crate::request::maximum_page_size(),"default":crate::request::DEFAULT_PAGE_SIZE}) }
            else { json!({"type":"string"}) };
        json!({"name":name,"in":"query","required":false,"schema":schema})
    }).collect()
}

fn command_example() -> Value {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    for field in ["schema_version", "origin", "admitted_at"] {
        value.as_object_mut().unwrap().remove(field);
    }
    value
}
