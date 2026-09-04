use axum::{
    Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uob_application::SnapshotQuery;
use uob_contracts::StationSnapshot;

use crate::{
    error::IntegrationErrorCode,
    request::{PageParameters, ResourceParameters, permits_read, require_reader, snapshot_cursor},
    routing::IntegrationState,
};

#[cfg(test)]
mod tests;

/// Bounded station inventory page.
#[derive(Serialize)]
pub(crate) struct StationPage {
    items: Vec<StationSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// Serves the paginated canonical station inventory.
///
/// The host's scoped port bounds the page and the configured target's own scope; this handler
/// then removes every station outside the calling credential's narrower scope, so enumeration
/// cannot reveal a station the caller could not address directly.
pub(crate) async fn stations(
    State(state): State<IntegrationState>,
    headers: HeaderMap,
    page: Result<Query<PageParameters>, QueryRejection>,
) -> Response {
    let Ok(permit) = state.acquire() else {
        return IntegrationErrorCode::CapacityExhausted.into_response();
    };
    let response = station_page(&state, &headers, page).await;
    drop(permit);
    response.map_or_else(IntoResponse::into_response, IntoResponse::into_response)
}

async fn station_page(
    state: &IntegrationState,
    headers: &HeaderMap,
    page: Result<Query<PageParameters>, QueryRejection>,
) -> Result<Json<StationPage>, IntegrationErrorCode> {
    let principal = require_reader(state.authenticate(headers)?)?;
    let Ok(Query(page)) = page else {
        return Err(IntegrationErrorCode::InvalidRequest);
    };
    let query = SnapshotQuery {
        after: snapshot_cursor(page.after.as_ref())?,
        limit: page.page_limit()?,
    };

    let page = state.reads().station_snapshots(query).await?;
    Ok(Json(StationPage {
        items: page
            .items
            .into_iter()
            .filter(|snapshot| permits_read(principal, &snapshot.station))
            .collect(),
        next_cursor: page.next_cursor.map(|cursor| cursor.as_str().to_owned()),
    }))
}

/// Serves one current canonical station snapshot.
///
/// A station outside the caller's scope is refused with the same permission code whether or not
/// it exists, so direct-identifier access cannot be used to enumerate other scopes.
pub(crate) async fn station(
    State(state): State<IntegrationState>,
    headers: HeaderMap,
    Path(station_id): Path<String>,
    selection: Result<Query<ResourceParameters>, QueryRejection>,
) -> Response {
    let Ok(permit) = state.acquire() else {
        return IntegrationErrorCode::CapacityExhausted.into_response();
    };
    let response = one_station(&state, &headers, &station_id, selection).await;
    drop(permit);
    response.map_or_else(IntoResponse::into_response, IntoResponse::into_response)
}

async fn one_station(
    state: &IntegrationState,
    headers: &HeaderMap,
    station_id: &str,
    selection: Result<Query<ResourceParameters>, QueryRejection>,
) -> Result<Json<StationSnapshot>, IntegrationErrorCode> {
    let principal = require_reader(state.authenticate(headers)?)?;
    let Ok(Query(selection)) = selection else {
        return Err(IntegrationErrorCode::InvalidRequest);
    };
    if selection.narrows_resource() {
        // A station snapshot is addressed by station identity alone; the EVSE and connector
        // filters belong to the point resource, and silently ignoring them would answer a
        // narrower question than the caller asked.
        return Err(IntegrationErrorCode::InvalidRequest);
    }
    let resource = selection.resource_for_station(principal, station_id)?;
    if !permits_read(principal, &resource) {
        return Err(IntegrationErrorCode::PermissionDenied);
    }

    let snapshot = state.reads().station_snapshot(resource).await?;
    let snapshot = snapshot.ok_or(IntegrationErrorCode::ResourceNotFound)?;
    if !permits_read(principal, &snapshot.station) {
        return Err(IntegrationErrorCode::PermissionDenied);
    }
    Ok(Json(snapshot))
}
