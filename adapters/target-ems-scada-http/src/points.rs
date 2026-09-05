use axum::{
    Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uob_application::{SnapshotQuery, TargetQuery};
use uob_contracts::{PointId, ResourceRef};

use crate::{
    configuration::IntegrationPrincipal,
    error::IntegrationErrorCode,
    reads::CanonicalRead,
    request::{
        PageParameters, ResourceParameters, intersects_station, permits_read, require_reader,
        snapshot_cursor,
    },
    routing::IntegrationState,
};

pub(crate) mod catalog;
mod cursor;
#[cfg(test)]
mod tests;

use catalog::PointView;
pub(crate) use cursor::POINT_CURSOR_PREFIX;
use cursor::PointCursor;

/// Filtered, paginated point query.
///
/// The fields are declared once rather than composed from the shared parameter structs: the
/// URL-encoded form is not self-describing, so a flattened struct cannot be deserialized from it.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PointPageParameters {
    after: Option<String>,
    limit: Option<u16>,
    bridge_id: Option<String>,
    station_id: Option<String>,
    evse_id: Option<String>,
    connector_id: Option<String>,
}

impl PointPageParameters {
    fn page(&self) -> PageParameters {
        PageParameters {
            after: None,
            limit: self.limit,
        }
    }

    fn selection(&self) -> ResourceParameters {
        ResourceParameters {
            bridge_id: self.bridge_id.clone(),
            station_id: self.station_id.clone(),
            evse_id: self.evse_id.clone(),
            connector_id: self.connector_id.clone(),
        }
    }
}

/// Bounded canonical point page.
#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct PointPage {
    items: Vec<PointView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// Serves the filtered, paginated canonical point catalog.
pub(crate) async fn points(
    State(state): State<IntegrationState>,
    headers: HeaderMap,
    parameters: Result<Query<PointPageParameters>, QueryRejection>,
) -> Response {
    let Ok(permit) = state.acquire() else {
        return IntegrationErrorCode::CapacityExhausted.into_response();
    };
    let response = point_page(&state, &headers, parameters).await;
    drop(permit);
    response.map_or_else(IntoResponse::into_response, IntoResponse::into_response)
}

async fn point_page(
    state: &IntegrationState,
    headers: &HeaderMap,
    parameters: Result<Query<PointPageParameters>, QueryRejection>,
) -> Result<Json<PointPage>, IntegrationErrorCode> {
    let principal = require_reader(state.authenticate(headers)?)?;
    let Ok(Query(parameters)) = parameters else {
        return Err(IntegrationErrorCode::InvalidRequest);
    };
    let limit = usize::from(parameters.page().page_limit()?.get());
    let cursor = parameters
        .after
        .as_deref()
        .map(PointCursor::decode)
        .transpose()?
        .unwrap_or_default();

    if parameters.station_id.is_some() {
        return station_points(state, principal, &parameters, cursor, limit).await;
    }
    if parameters.selection().narrows_resource() {
        // An EVSE or connector identifier is only meaningful below a named station.
        return Err(IntegrationErrorCode::InvalidRequest);
    }
    bridge_points(state, principal, cursor, limit).await
}

/// Serves the points of one named station, optionally narrowed to one EVSE or connector.
///
/// A station that does not exist and a station outside the caller's scope both produce an empty
/// page, so a point filter cannot be used to discover which stations exist elsewhere.
async fn station_points(
    state: &IntegrationState,
    principal: &IntegrationPrincipal,
    parameters: &PointPageParameters,
    cursor: PointCursor,
    limit: usize,
) -> Result<Json<PointPage>, IntegrationErrorCode> {
    let selection = parameters.selection();
    let addressed = selection.resource(principal)?;
    let station = station_reference(&addressed);
    let available = if intersects_station(principal, &station) {
        state
            .reads()
            .station_snapshot(station)
            .await?
            .map(|snapshot| {
                catalog::points(&snapshot, addressed.resource.as_ref())
                    .into_iter()
                    .filter(|point| permits_read(principal, &point.resource))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        // A station holding nothing for this credential is answered without a canonical read, so
        // an absent station and one belonging to another scope look exactly alike.
        Vec::new()
    };

    let delivered = cursor.delivered;
    let total = available.len();
    let items = window(available, delivered, limit);
    let consumed = delivered.saturating_add(items.len());
    let next_cursor = (consumed < total).then(|| PointCursor::resume(consumed, None).encode());
    Ok(Json(PointPage { items, next_cursor }))
}

/// Serves points across every station the calling credential can read.
///
/// One bounded station-snapshot read backs each page. The page is bounded by the requested point
/// limit, and the number of snapshots inspected per request is bounded by the listener's own scan
/// budget, so neither a wide installation nor a deep station can produce an unbounded response.
async fn bridge_points(
    state: &IntegrationState,
    principal: &IntegrationPrincipal,
    cursor: PointCursor,
    limit: usize,
) -> Result<Json<PointPage>, IntegrationErrorCode> {
    let page = state
        .reads()
        .station_snapshots(SnapshotQuery {
            after: snapshot_cursor(cursor.after.as_ref())?,
            limit: state.station_scan_limit(),
        })
        .await?;

    let available: Vec<PointView> = page
        .items
        .iter()
        .flat_map(|snapshot| catalog::points(snapshot, None))
        .filter(|point| permits_read(principal, &point.resource))
        .collect();

    let delivered = cursor.delivered;
    let total = available.len();
    let items = window(available, delivered, limit);
    let consumed = delivered.saturating_add(items.len());
    let next_cursor = if consumed < total {
        Some(PointCursor::resume(consumed, cursor.after).encode())
    } else {
        page.next_cursor
            .map(|next| PointCursor::advance(next.as_str().to_owned()).encode())
    };
    Ok(Json(PointPage { items, next_cursor }))
}

/// Serves one canonical point through the host's explicit descriptor and value queries.
pub(crate) async fn point(
    State(state): State<IntegrationState>,
    headers: HeaderMap,
    point_id: Result<Path<String>, axum::extract::rejection::PathRejection>,
    selection: Result<Query<ResourceParameters>, QueryRejection>,
) -> Response {
    let Ok(permit) = state.acquire() else {
        return IntegrationErrorCode::CapacityExhausted.into_response();
    };
    let Ok(Path(point_id)) = point_id else {
        return IntegrationErrorCode::InvalidRequest.into_response();
    };
    let response = one_point(&state, &headers, &point_id, selection).await;
    drop(permit);
    response.map_or_else(IntoResponse::into_response, IntoResponse::into_response)
}

async fn one_point(
    state: &IntegrationState,
    headers: &HeaderMap,
    point_id: &str,
    selection: Result<Query<ResourceParameters>, QueryRejection>,
) -> Result<Json<PointView>, IntegrationErrorCode> {
    let principal = require_reader(state.authenticate(headers)?)?;
    let Ok(Query(selection)) = selection else {
        return Err(IntegrationErrorCode::InvalidRequest);
    };
    let point_id = PointId::new(point_id).map_err(|_| IntegrationErrorCode::InvalidRequest)?;
    let resource = selection.resource(principal)?;
    if !permits_read(principal, &resource) {
        return Err(IntegrationErrorCode::PermissionDenied);
    }

    let descriptor = match state
        .reads()
        .read(TargetQuery::DataPointDescriptor {
            resource: resource.clone(),
            point_id: point_id.clone(),
        })
        .await?
    {
        CanonicalRead::DataPointDescriptor(descriptor) => *descriptor,
        _ => return Err(IntegrationErrorCode::SourceUnavailable),
    };
    let value = match state
        .reads()
        .read(TargetQuery::DataPointValue {
            resource: resource.clone(),
            point_id: point_id.clone(),
        })
        .await?
    {
        CanonicalRead::DataPointValue(value) => *value,
        _ => return Err(IntegrationErrorCode::SourceUnavailable),
    };

    if descriptor.is_none() && value.is_none() {
        return Err(IntegrationErrorCode::ResourceNotFound);
    }
    Ok(Json(PointView {
        point_id,
        // The canonical descriptor carries the retained native protocol address; the reference
        // rebuilt from request parameters cannot.
        resource: descriptor
            .as_ref()
            .map_or(resource, |descriptor| descriptor.resource.clone()),
        descriptor,
        value,
    }))
}

/// Returns the station-level reference behind an addressed resource.
fn station_reference(resource: &ResourceRef) -> ResourceRef {
    ResourceRef {
        bridge_id: resource.bridge_id.clone(),
        station_id: resource.station_id.clone(),
        resource: None,
        native_protocol_reference: None,
    }
}

/// Takes the bounded page starting after the already-delivered points.
fn window(available: Vec<PointView>, delivered: usize, limit: usize) -> Vec<PointView> {
    available.into_iter().skip(delivered).take(limit).collect()
}
