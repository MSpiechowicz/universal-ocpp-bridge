mod payload;
mod recovery;
mod request;
mod subscriber;

#[cfg(test)]
mod tests;

use std::{collections::BTreeSet, convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures_util::stream;
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use uob_application::{
    CanonicalQuerySource, PageLimit, RetainedEventCursor, RetainedEventItem, RetainedEventQuery,
    ScopedTargetQueryPort, TargetPortError, TargetPortErrorCode, TargetQuery,
    TargetQueryAuthorization, TargetQueryPermission, TargetQueryPort, TargetQueryResult,
    TargetRetainedEventStream, WorkClass,
};
use uob_contracts::ResourceRef;

use crate::{ManagementReadLimits, ManagementState, read_api::ApiError};
use payload::PayloadEncodingError;
use recovery::{cursor_expired_response, error_event, gap_event};
use request::{EventQuery, bearer_token, validate_query};
use subscriber::{QueuedPayloadReservation, SubscriberBudget};

const MAX_SSE_ID_BYTES: usize = 512;
const HARD_MAX_SUBSCRIBERS: usize = 128;
const HARD_MAX_PAYLOAD_BYTES: usize = 256 * 1024;
const HARD_MAX_OPERATION_TIMEOUT: Duration = Duration::from_secs(60);
const HARD_MAX_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// Trusted event access returned only after transport authentication succeeds.
#[derive(Clone)]
pub struct AuthenticatedEventAccess {
    /// Read grant attached to the principal.
    ///
    /// It must permit retained events for the selected resource and station snapshots for its
    /// containing station, so the same credential can recover from every advertised cursor gap.
    pub authorization: TargetQueryAuthorization,
    /// Exact resource used when a client, including `uob events`, omits a filter.
    ///
    /// Native protocol addressing is discarded because it is neither a canonical filter nor safe
    /// recovery metadata.
    pub default_resource: ResourceRef,
}

/// Adapter-owned bearer authentication boundary for the event endpoint.
///
/// Implementations resolve protected credential material outside safe configuration and compare
/// secrets in constant time. The request token is never passed to application or storage ports.
pub trait ManagementEventAuthenticator: Send + Sync {
    /// Authenticates one syntactically valid bearer token and returns its immutable access grant.
    fn authenticate(&self, bearer_token: &str) -> Option<AuthenticatedEventAccess>;
}

/// Host-owned limits for management event subscriptions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementEventLimits {
    /// Maximum event-stream producers active at once.
    pub maximum_concurrent_subscribers: usize,
    /// Maximum queued SSE records per subscriber and requested source subscription capacity.
    pub subscriber_capacity: u16,
    /// Deadline for opening an application-owned retained stream.
    pub subscription_timeout: Duration,
    /// Maximum time a full HTTP subscriber queue may remain blocked before disconnect.
    pub slow_subscriber_timeout: Duration,
    /// Maximum serialized canonical envelope size.
    pub maximum_payload_bytes: usize,
    /// Interval for Axum SSE keep-alive comments.
    pub keep_alive_interval: Duration,
}

impl Default for ManagementEventLimits {
    fn default() -> Self {
        Self {
            maximum_concurrent_subscribers: 32,
            subscriber_capacity: 32,
            subscription_timeout: Duration::from_secs(2),
            slow_subscriber_timeout: Duration::from_secs(5),
            maximum_payload_bytes: HARD_MAX_PAYLOAD_BYTES,
            keep_alive_interval: Duration::from_secs(15),
        }
    }
}

/// Dependencies required to enable authenticated durable events on a router.
#[derive(Clone)]
pub struct ManagementEventConfiguration {
    /// Bearer-token verifier that supplies trusted per-principal resource scopes.
    pub authenticator: Arc<dyn ManagementEventAuthenticator>,
    /// Per-router subscription limits.
    pub limits: ManagementEventLimits,
}

#[derive(Clone)]
pub(crate) struct ManagementEvents {
    source: Arc<dyn CanonicalQuerySource<Value>>,
    authenticator: Arc<dyn ManagementEventAuthenticator>,
    permits: Arc<Semaphore>,
    limits: ManagementEventLimits,
    read_permits: Arc<Semaphore>,
    read_timeout: Duration,
}

impl ManagementEvents {
    pub(crate) fn new(
        source: Arc<dyn CanonicalQuerySource<Value>>,
        configuration: ManagementEventConfiguration,
        read_limits: ManagementReadLimits,
    ) -> Self {
        Self {
            source,
            authenticator: configuration.authenticator,
            permits: Arc::new(Semaphore::new(
                configuration
                    .limits
                    .maximum_concurrent_subscribers
                    .min(HARD_MAX_SUBSCRIBERS + 1),
            )),
            limits: configuration.limits,
            read_permits: Arc::new(Semaphore::new(read_limits.maximum_concurrent_queries)),
            read_timeout: read_limits.query_timeout,
        }
    }

    pub(crate) fn authenticate(&self, headers: &HeaderMap) -> Result<AuthenticatedEventAccess, ()> {
        let token = bearer_token(headers)?;
        self.authenticator.authenticate(token).ok_or(())
    }

    pub(crate) async fn query(
        &self,
        authorization: TargetQueryAuthorization,
        query: TargetQuery,
    ) -> Result<TargetQueryResult<Value>, ApiError> {
        let permit = self
            .read_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Busy)?;
        let port = ScopedTargetQueryPort::new(Arc::clone(&self.source), authorization);
        let result = tokio::time::timeout(self.read_timeout, port.query(query))
            .await
            .map_err(|_| ApiError::Timeout)?
            .map_err(ApiError::Port);
        drop(permit);
        result
    }
}

pub(crate) async fn events(
    State(state): State<ManagementState>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Response {
    let Some(events) = state.events else {
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "events.unavailable");
    };
    if !valid_limits(events.limits) {
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "events.invalid_limits");
    }
    let Ok(access) = events.authenticate(&headers) else {
        return authentication_error();
    };
    let query = match validate_query(query, &headers, &state.application, &access) {
        Ok(query) => query,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code),
    };
    if query.resource.bridge_id != state.application.identity().bridge_id {
        return api_error(StatusCode::FORBIDDEN, "events.resource_unauthorized");
    }
    if !recovery_authorized(&access.authorization, &query.resource) {
        return api_error(
            StatusCode::FORBIDDEN,
            "events.snapshot_recovery_not_granted",
        );
    }
    let Ok(permit) = events.permits.clone().try_acquire_owned() else {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "events.subscriber_limit");
    };
    let Ok(connection_reservation) = state
        .application
        .health()
        .resources()
        .try_reserve(WorkClass::Subscriber, 0)
    else {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "events.subscriber_limit");
    };
    let subscriber_budget = Arc::new(SubscriberBudget::new(connection_reservation));
    let port = ScopedTargetQueryPort::new(Arc::clone(&events.source), access.authorization);
    let subscription = port.subscribe_retained_events(RetainedEventQuery {
        resource: query.resource.clone(),
        after: query.after.clone(),
        limit: PageLimit::new(events.limits.subscriber_capacity).expect("validated event capacity"),
    });
    let subscription =
        match tokio::time::timeout(events.limits.subscription_timeout, subscription).await {
            Ok(Ok(subscription)) => subscription,
            Ok(Err(error)) if error.code() == TargetPortErrorCode::CursorExpired => {
                return cursor_expired_response(&query.resource, &query.event_types);
            }
            Ok(Err(error)) => return port_error(&error),
            Err(_) => return api_error(StatusCode::GATEWAY_TIMEOUT, "events.subscription_timeout"),
        };

    let (sender, receiver) = mpsc::channel(usize::from(events.limits.subscriber_capacity));
    tokio::spawn(pump(
        subscription,
        sender,
        query.resource,
        query.event_types,
        events.limits,
        permit,
        subscriber_budget,
    ));
    let output = stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|queued| {
            let QueuedEvent {
                event,
                reservation: _reservation,
            } = queued;
            (Ok::<Event, Infallible>(event), receiver)
        })
    });
    Sse::new(output)
        .keep_alive(
            KeepAlive::new()
                .interval(events.limits.keep_alive_interval)
                .text("keep-alive"),
        )
        .into_response()
}

async fn pump(
    mut source: TargetRetainedEventStream<Value>,
    sender: mpsc::Sender<QueuedEvent>,
    resource: ResourceRef,
    event_types: BTreeSet<String>,
    limits: ManagementEventLimits,
    _permit: OwnedSemaphorePermit,
    subscriber_budget: Arc<SubscriberBudget>,
) {
    loop {
        let item = tokio::select! {
            biased;
            () = sender.closed() => return,
            item = std::future::poll_fn(|context| source.as_mut().poll_event(context)) => item,
        };
        let event = match item {
            Some(Ok(item)) => {
                match encode_item(&item, &event_types, limits.maximum_payload_bytes) {
                    Ok(event) => event,
                    Err(event) => {
                        let _ = send(
                            &sender,
                            event,
                            limits.slow_subscriber_timeout,
                            &subscriber_budget,
                        )
                        .await;
                        return;
                    }
                }
            }
            Some(Err(error)) => {
                let _ = send(
                    &sender,
                    stream_error_event(&error, &resource, &event_types),
                    limits.slow_subscriber_timeout,
                    &subscriber_budget,
                )
                .await;
                return;
            }
            None => return,
        };
        if !send(
            &sender,
            event,
            limits.slow_subscriber_timeout,
            &subscriber_budget,
        )
        .await
        {
            return;
        }
    }
}

struct QueuedEvent {
    event: Event,
    reservation: QueuedPayloadReservation,
}

async fn send(
    sender: &mpsc::Sender<QueuedEvent>,
    (event, encoded_bytes): (Event, usize),
    timeout: Duration,
    budget: &Arc<SubscriberBudget>,
) -> bool {
    let Some(reservation) = budget.reserve(encoded_bytes) else {
        return false;
    };
    matches!(
        tokio::time::timeout(timeout, sender.send(QueuedEvent { event, reservation })).await,
        Ok(Ok(()))
    )
}

fn encode_item(
    item: &RetainedEventItem<Value>,
    event_types: &BTreeSet<String>,
    maximum_payload_bytes: usize,
) -> Result<(Event, usize), (Event, usize)> {
    let cursor =
        safe_sse_id(&item.cursor).ok_or_else(|| error_event("events.invalid_source_cursor"))?;
    if !matches_event_type(event_types, item.event.event_type.as_str()) {
        return Ok((Event::default().id(cursor), cursor.len() + 6));
    }
    let payload = payload::encode_json(&item.event, maximum_payload_bytes).map_err(|error| {
        let code = match error {
            PayloadEncodingError::TooLarge => "events.payload_too_large",
            PayloadEncodingError::Serialization => "events.serialization_failed",
        };
        error_event(code)
    })?;
    let encoded_bytes = payload.len() + cursor.len() + 28;
    Ok((
        Event::default().event("durable").id(cursor).data(payload),
        encoded_bytes,
    ))
}

fn matches_event_type(event_types: &BTreeSet<String>, event_type: &str) -> bool {
    event_types.is_empty() || event_types.contains(event_type)
}

fn stream_error_event(
    error: &TargetPortError,
    resource: &ResourceRef,
    event_types: &BTreeSet<String>,
) -> (Event, usize) {
    if error.code() == TargetPortErrorCode::CursorExpired {
        gap_event(resource, event_types)
    } else {
        error_event(match error.code() {
            TargetPortErrorCode::Unauthorized => "events.resource_unauthorized",
            TargetPortErrorCode::Unsupported => "events.operation_not_granted",
            TargetPortErrorCode::Expired => "events.request_expired",
            TargetPortErrorCode::Busy => "events.source_busy",
            TargetPortErrorCode::Unavailable => "events.source_unavailable",
            TargetPortErrorCode::InvalidRequest => "events.invalid_source_event",
            TargetPortErrorCode::CursorExpired => unreachable!(),
        })
    }
}

fn safe_sse_id(cursor: &RetainedEventCursor) -> Option<&str> {
    let value = cursor.as_str();
    (value.len() <= MAX_SSE_ID_BYTES && !unsafe_text(value)).then_some(value)
}

fn unsafe_text(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn valid_limits(limits: ManagementEventLimits) -> bool {
    (1..=HARD_MAX_SUBSCRIBERS).contains(&limits.maximum_concurrent_subscribers)
        && PageLimit::new(limits.subscriber_capacity).is_ok()
        && (1..=HARD_MAX_PAYLOAD_BYTES).contains(&limits.maximum_payload_bytes)
        && !limits.subscription_timeout.is_zero()
        && limits.subscription_timeout <= HARD_MAX_OPERATION_TIMEOUT
        && !limits.slow_subscriber_timeout.is_zero()
        && limits.slow_subscriber_timeout <= HARD_MAX_OPERATION_TIMEOUT
        && !limits.keep_alive_interval.is_zero()
        && limits.keep_alive_interval <= HARD_MAX_KEEP_ALIVE_INTERVAL
}

fn recovery_authorized(authorization: &TargetQueryAuthorization, resource: &ResourceRef) -> bool {
    let station = ResourceRef {
        bridge_id: resource.bridge_id.clone(),
        station_id: resource.station_id.clone(),
        resource: None,
        native_protocol_reference: None,
    };
    authorization.permits(TargetQueryPermission::RetainedEvents)
        && authorization.permits(TargetQueryPermission::StationSnapshots)
        && authorization.permits_resource(resource)
        && authorization.permits_resource(&station)
}

pub(crate) fn authentication_error() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        axum::Json(serde_json::json!({ "error": "events.authentication_required" })),
    )
        .into_response()
}

fn port_error(error: &TargetPortError) -> Response {
    let (status, code) = match error.code() {
        TargetPortErrorCode::Unauthorized => {
            (StatusCode::FORBIDDEN, "events.resource_unauthorized")
        }
        TargetPortErrorCode::Unsupported => (StatusCode::FORBIDDEN, "events.operation_not_granted"),
        TargetPortErrorCode::Expired => (StatusCode::GONE, "events.request_expired"),
        TargetPortErrorCode::Busy => (StatusCode::TOO_MANY_REQUESTS, "events.source_busy"),
        TargetPortErrorCode::Unavailable => {
            (StatusCode::SERVICE_UNAVAILABLE, "events.source_unavailable")
        }
        TargetPortErrorCode::InvalidRequest => (StatusCode::BAD_REQUEST, "events.invalid_request"),
        TargetPortErrorCode::CursorExpired => (StatusCode::GONE, "events.cursor_expired"),
    };
    api_error(status, code)
}

fn api_error(status: StatusCode, code: &'static str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": code }))).into_response()
}
