use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uob_application::{
    CommandHistoryCursor, CommandHistoryQuery, PageLimit, TargetQuery, TargetQueryResult,
};
use uob_contracts::{RequestId, ResourceRef, StationId};

use super::{authentication_error, error};
use crate::ManagementState;

pub(crate) async fn status(
    State(state): State<ManagementState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Response {
    if state.queries.is_none() && state.events.is_none() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "command.status_unavailable",
        );
    }
    // Query-backed reads still bind the result to the command caller. Event-backed reads use
    // their independent station-scoped read grant, never the submission credential.
    let origin = if state.events.is_none() {
        let Some(commands) = &state.commands else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "command.status_unavailable",
            );
        };
        let Ok(origin) = commands.authenticate(&headers) else {
            return authentication_error();
        };
        Some(origin)
    } else {
        None
    };
    let Ok(request_id) = RequestId::new(request_id) else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_request_id");
    };
    match crate::read_api::execute_query(&state, &headers, TargetQuery::CommandResult(request_id))
        .await
    {
        Ok(TargetQueryResult::CommandResult(Some(result))) => {
            if origin
                .as_ref()
                .is_some_and(|origin| *origin != result.return_route.origin)
            {
                return error(StatusCode::FORBIDDEN, "command.unauthorized");
            }
            Json(result).into_response()
        }
        Ok(TargetQueryResult::CommandResult(None)) => {
            error(StatusCode::NOT_FOUND, "command.not_found")
        }
        Ok(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "command.response_type_mismatch",
        ),
        Err(value) => value.into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct CommandPageQuery {
    station_id: String,
    limit: Option<u16>,
    after: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct CommandSchemaQuery {
    station_id: String,
}

pub(crate) async fn history(
    State(state): State<ManagementState>,
    headers: HeaderMap,
    Query(query): Query<CommandPageQuery>,
) -> Response {
    let Ok(station) = station_resource(&state, query.station_id) else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_station_id");
    };
    let Ok(limit) = PageLimit::new(query.limit.unwrap_or(25)) else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_page_limit");
    };
    let Ok(after) = query.after.map(CommandHistoryCursor::new).transpose() else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_cursor");
    };
    if state.events.is_none() {
        let Some(commands) = &state.commands else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "command.status_unavailable",
            );
        };
        let Ok(origin) = commands.authenticate(&headers) else {
            return authentication_error();
        };
        if !commands.authenticator.permits_schema(&origin, &station) {
            return error(StatusCode::FORBIDDEN, "command.unauthorized");
        }
    }

    match crate::read_api::execute_query(
        &state,
        &headers,
        TargetQuery::CommandHistory(CommandHistoryQuery {
            station,
            after,
            limit,
        }),
    )
    .await
    {
        Ok(TargetQueryResult::CommandHistory(page)) => Json(serde_json::json!({
            "items": page.items,
            "next_cursor": page.next_cursor.map(|cursor| cursor.as_str().to_owned()),
        }))
        .into_response(),
        Ok(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "command.response_type_mismatch",
        ),
        Err(value) => value.into_response(),
    }
}

pub(crate) async fn schemas(
    State(state): State<ManagementState>,
    headers: HeaderMap,
    Query(query): Query<CommandSchemaQuery>,
) -> Response {
    let Ok(station) = station_resource(&state, query.station_id) else {
        return error(StatusCode::BAD_REQUEST, "command.invalid_station_id");
    };
    let Some(commands) = &state.commands else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "command.admission_unavailable",
        );
    };
    let Ok(origin) = commands.authenticate(&headers) else {
        return authentication_error();
    };
    if !commands.authenticator.permits_schema(&origin, &station) {
        return error(StatusCode::FORBIDDEN, "command.unauthorized");
    }
    match commands.station_snapshot(station.clone()).await {
        Ok(Some(snapshot)) => {
            let start = commands
                .authenticator
                .start_reference(&origin, &station)
                .map(|authorization_reference| {
                    serde_json::json!({
                        "resource": station, "authorization_reference": authorization_reference,
                    })
                });
            Json(serde_json::json!({
                "items": commands.privileged_payloads.schemas(&snapshot),
                "start": start,
            }))
            .into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "command.station_not_found"),
        Err(response) => *response,
    }
}

fn station_resource(state: &ManagementState, station_id: String) -> Result<ResourceRef, ()> {
    Ok(ResourceRef {
        bridge_id: state.application.identity().bridge_id.clone(),
        station_id: StationId::new(station_id).map_err(|_| ())?,
        resource: None,
        native_protocol_reference: None,
    })
}
