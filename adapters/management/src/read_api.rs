use std::{sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use uob_application::{
    CanonicalQuerySource, PageLimit, ScopedTargetQueryPort, SnapshotCursor, SnapshotQuery,
    TargetPortError, TargetPortErrorCode, TargetQuery, TargetQueryAuthorization, TargetQueryPort,
    TargetQueryResult,
};
use uob_contracts::{ResourceRef, StationId, StationSnapshot};

use crate::ManagementState;

const DEFAULT_PAGE_SIZE: u16 = 25;

/// Host-owned bounds for management reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementReadLimits {
    /// Maximum reads allowed to wait on the canonical source concurrently.
    pub maximum_concurrent_queries: usize,
    /// Deadline applied independently to every canonical source call.
    pub query_timeout: Duration,
}

impl Default for ManagementReadLimits {
    fn default() -> Self {
        Self {
            maximum_concurrent_queries: 8,
            query_timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ManagementQueries {
    port: Arc<dyn TargetQueryPort<serde_json::Value>>,
    permits: Arc<Semaphore>,
    timeout: Duration,
}

impl ManagementQueries {
    pub(crate) fn new(
        source: Arc<dyn CanonicalQuerySource<serde_json::Value>>,
        authorization: TargetQueryAuthorization,
        limits: ManagementReadLimits,
    ) -> Self {
        Self {
            port: Arc::new(ScopedTargetQueryPort::new(source, authorization)),
            permits: Arc::new(Semaphore::new(limits.maximum_concurrent_queries)),
            timeout: limits.query_timeout,
        }
    }

    pub(crate) async fn query(
        &self,
        query: TargetQuery,
    ) -> Result<TargetQueryResult<serde_json::Value>, ApiError> {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Busy)?;
        let result = tokio::time::timeout(self.timeout, self.port.query(query))
            .await
            .map_err(|_| ApiError::Timeout)?
            .map_err(ApiError::Port);
        drop(permit);
        result
    }
}

#[derive(Deserialize)]
pub(crate) struct StationPageQuery {
    after: Option<String>,
    limit: Option<u16>,
}

#[derive(Serialize)]
struct StationPage {
    items: Vec<StationSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

pub(crate) async fn stations(
    State(state): State<ManagementState>,
    headers: HeaderMap,
    Query(query): Query<StationPageQuery>,
) -> Response {
    let Ok(limit) = PageLimit::new(query.limit.unwrap_or(DEFAULT_PAGE_SIZE)) else {
        return ApiError::Invalid("query.invalid_page_limit").into_response();
    };
    let Ok(after) = query.after.map(SnapshotCursor::new).transpose() else {
        return ApiError::Invalid("query.invalid_cursor").into_response();
    };
    match execute_query(
        &state,
        &headers,
        TargetQuery::StationSnapshots(SnapshotQuery { after, limit }),
    )
    .await
    {
        Ok(TargetQueryResult::StationSnapshots(page)) => Json(StationPage {
            items: page.items,
            next_cursor: page.next_cursor.map(|cursor| cursor.as_str().to_owned()),
        })
        .into_response(),
        Ok(_) => ApiError::Invalid("query.response_type_mismatch").into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn station(
    State(state): State<ManagementState>,
    headers: HeaderMap,
    Path(station_id): Path<String>,
) -> Response {
    let Ok(station_id) = StationId::new(station_id) else {
        return ApiError::Invalid("query.invalid_station_id").into_response();
    };
    let resource = ResourceRef {
        bridge_id: state.application.identity().bridge_id.clone(),
        station_id,
        resource: None,
        native_protocol_reference: None,
    };
    match execute_query(&state, &headers, TargetQuery::StationSnapshot(resource)).await {
        Ok(TargetQueryResult::StationSnapshot(Some(snapshot))) => Json(snapshot).into_response(),
        Ok(TargetQueryResult::StationSnapshot(None)) => ApiError::NotFound.into_response(),
        Ok(_) => ApiError::Invalid("query.response_type_mismatch").into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn execute_query(
    state: &ManagementState,
    headers: &HeaderMap,
    query: TargetQuery,
) -> Result<TargetQueryResult<serde_json::Value>, ApiError> {
    if let Some(events) = &state.events {
        let access = events
            .authenticate(headers)
            .map_err(|()| ApiError::Authentication)?;
        return events.query(access.authorization, query).await;
    }
    let Some(queries) = &state.queries else {
        return Err(ApiError::Unavailable);
    };
    queries.query(query).await
}

pub(crate) enum ApiError {
    Authentication,
    Invalid(&'static str),
    NotFound,
    Busy,
    Timeout,
    Unavailable,
    Port(TargetPortError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if matches!(&self, Self::Authentication) {
            return crate::event_api::authentication_error();
        }
        let (status, code) = match self {
            Self::Authentication => unreachable!(),
            Self::Invalid(code) => (StatusCode::BAD_REQUEST, code.to_owned()),
            Self::NotFound => (StatusCode::NOT_FOUND, "query.station_not_found".to_owned()),
            Self::Busy => (
                StatusCode::TOO_MANY_REQUESTS,
                "query.concurrency_limit".to_owned(),
            ),
            Self::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "query.deadline_exceeded".to_owned(),
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "query.source_unavailable".to_owned(),
            ),
            Self::Port(error) => match error.code() {
                TargetPortErrorCode::Unauthorized => {
                    (StatusCode::FORBIDDEN, error.context().to_owned())
                }
                TargetPortErrorCode::Unsupported => {
                    (StatusCode::NOT_IMPLEMENTED, error.context().to_owned())
                }
                TargetPortErrorCode::Busy => {
                    (StatusCode::TOO_MANY_REQUESTS, error.context().to_owned())
                }
                TargetPortErrorCode::Unavailable => {
                    (StatusCode::SERVICE_UNAVAILABLE, error.context().to_owned())
                }
                TargetPortErrorCode::InvalidRequest | TargetPortErrorCode::CursorExpired => {
                    (StatusCode::BAD_REQUEST, error.context().to_owned())
                }
                TargetPortErrorCode::Expired => (StatusCode::GONE, error.context().to_owned()),
            },
        };
        (status, Json(serde_json::json!({ "error": code }))).into_response()
    }
}
