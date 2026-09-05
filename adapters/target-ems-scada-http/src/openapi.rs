//! Versioned HTTP contract, generated from the shipped wire models and route inventory.
mod operations;
mod schemas;
#[cfg(test)]
pub(crate) mod tests;

use crate::{error::IntegrationErrorCode as Error, routing::IntegrationState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};

/// Generates the integration contract for publication and offline drift verification.
/// Canonical references are same-origin URLs and work without an external schema registry.
#[must_use]
pub fn openapi_document() -> Value {
    let mut schemas = Map::new();
    schemas::add::<crate::capabilities::CapabilityDocument>(
        &mut schemas,
        "CapabilityDocument",
        true,
    );
    schemas::add::<crate::stations::StationPage>(&mut schemas, "StationPage", true);
    schemas::add::<crate::points::PointPage>(&mut schemas, "PointPage", true);
    schemas::add::<crate::points::catalog::PointView>(&mut schemas, "PointView", true);
    schemas::add::<crate::commands::AcceptedCommand>(&mut schemas, "AcceptedCommand", true);
    schemas::add::<uob_contracts::CommandRequest<Value>>(&mut schemas, "CommandRequest", false);
    schemas.insert("IntegrationError".into(), json!({"type":"object", "required":["error"],
        "properties":{"error":{"type":"string", "enum":Error::ALL.iter().map(|e|e.as_str()).collect::<Vec<_>>()}}}));
    schemas.insert("CursorGap".into(), json!({"type":"object", "required":["error","kind","recovery"],
        "properties":{"error":{"const":"ems_scada_http.cursor_expired"}, "kind":{"const":"durable_cursor_gap"},
        "recovery":{"type":"object", "required":["action","snapshot_url","resubscribe_url","resource","types","omit_cursor"],
        "properties":{"action":{"const":"fetch_fresh_snapshot"}, "snapshot_url":{"type":"string"},
        "resubscribe_url":{"type":"string"}, "resource":schemas::reference("resource-ref"),
        "types":{"type":"array","items":{"type":"string"}}, "omit_cursor":{"const":true}}}}}));
    let mut paths = Map::new();
    for resource in crate::capabilities::IMPLEMENTED_RESOURCES {
        paths.insert(resource.path.to_owned(), operations::path(resource.name));
    }
    json!({
        "openapi":"3.1.1", "jsonSchemaDialect":"https://json-schema.org/draft/2020-12/schema",
        "info":{"title":"Universal OCPP Bridge EMS/SCADA HTTP API", "version":"1.0.0",
            "description":include_str!("../openapi/semantics.md")},
        "servers":[{"url":"/", "description":"Selected EMS/SCADA integration listener; HTTPS outside isolated demos."}],
        "security":[{"integrationBearer":[]}],
        "paths":paths,
        "components":{"securitySchemes":{"integrationBearer":{"type":"http","scheme":"bearer",
            "description":"Opaque integration token from the configured credential file, scoped by role and canonical resource. Never a management credential."}},
            "schemas":schemas},
        "x-uob-contract-version":{"major":1,"revision":0},
        "x-uob-event-policy":crate::events::EventPolicy::default(),
        "x-uob-fallback-errors":{"unknown_path":{"status":404,"error":Error::UnknownResource.as_str()},
            "unsupported_method":{"status":405,"error":Error::UnsupportedOperation.as_str()}}
    })
}

pub(crate) async fn document(
    State(state): State<IntegrationState>,
    headers: HeaderMap,
) -> Response {
    let Ok(_permit) = state.acquire() else {
        return Error::CapacityExhausted.into_response();
    };
    if let Err(error) = state.authenticate(&headers) {
        return error.into_response();
    }
    (
        [(header::CONTENT_TYPE, "application/json")],
        include_str!("../openapi/v1.json"),
    )
        .into_response()
}

pub(crate) async fn schema(
    State(state): State<IntegrationState>,
    headers: HeaderMap,
    path: Result<Path<String>, axum::extract::rejection::PathRejection>,
) -> Response {
    let Ok(_permit) = state.acquire() else {
        return Error::CapacityExhausted.into_response();
    };
    if let Err(error) = state.authenticate(&headers) {
        return error.into_response();
    }
    let Ok(Path(name)) = path else {
        return Error::InvalidRequest.into_response();
    };
    match schemas::CANONICAL.iter().find(|(file, _)| *file == name) {
        Some((_, body)) => {
            ([(header::CONTENT_TYPE, "application/schema+json")], *body).into_response()
        }
        None => Error::UnknownResource.into_response(),
    }
}
