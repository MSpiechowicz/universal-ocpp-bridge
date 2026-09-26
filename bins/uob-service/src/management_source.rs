//! Canonical, station-scoped management reads from the live operational SQLite worker.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use serde_json::Value;
use tokio::time::{self, Sleep};
use uob_application::{
    CanonicalQuerySource, OperationalStore, Page, PageLimit, RetainedEventCursor,
    RetainedEventItem, RetainedEventPage, RetainedEventQuery, StationEvent, StorageError,
    StorageErrorCode, StorageFuture, TargetPortError, TargetPortErrorCode, TargetPortFuture,
    TargetQuery, TargetQueryAuthorization, TargetQueryResult, TargetRetainedEventStream,
    TargetSubscription,
};
use uob_contracts::{EventEnvelope, NativeProtocolReference, ResourceRef, TransactionSnapshot};
use uob_storage_adapter::SqliteOperationalStore;

/// The same worker handle used for charger commits; clones do not open a second database.
pub(crate) type ManagementStore =
    SqliteOperationalStore<Value, StationEvent, TransactionSnapshot, String>;

pub(crate) struct ManagementSource {
    store: ManagementStore,
}

impl ManagementSource {
    pub(crate) fn new(store: ManagementStore) -> Self {
        Self { store }
    }

    async fn query_retained_events(
        &self,
        authorization: &TargetQueryAuthorization,
        query: RetainedEventQuery,
    ) -> Result<TargetQueryResult<Value>, TargetPortError> {
        require_grant(authorization, &query.resource)?;
        let resource = query.resource.clone();
        let limit = query.limit;
        let RetainedEventPage {
            events,
            resume_cursor,
            has_more,
        } = self
            .store
            .read_retained_events(query)
            .await
            .map_err(|error| storage_error(&error))?;
        if events.len() > usize::from(limit.get())
            || (has_more && events.is_empty())
            || (!events.is_empty() && resume_cursor.is_none())
        {
            return Err(invalid_result());
        }
        if events
            .iter()
            .any(|event| !same_resource(&event.resource, &resource))
        {
            return Err(outside_scope());
        }
        let next_cursor = if has_more {
            Some(resume_cursor.ok_or_else(invalid_result)?)
        } else {
            None
        };
        let items = events
            .into_iter()
            .map(json_event)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TargetQueryResult::RetainedEvents(Page {
            items,
            next_cursor,
        }))
    }
}

impl CanonicalQuerySource<Value> for ManagementSource {
    fn query<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<Value>> {
        Box::pin(async move {
            match query {
                TargetQuery::StationSnapshot(station) => {
                    require_station(&station)?;
                    if !authorization
                        .station_resources()
                        .any(|granted| same_station(&granted, &station))
                    {
                        return Err(outside_scope());
                    }
                    let snapshot = self
                        .store
                        .station_snapshot(station.clone())
                        .await
                        .map_err(|error| storage_error(&error))?;
                    if snapshot.as_ref().is_some_and(|snapshot| {
                        !station_ref(&snapshot.station)
                            || !same_station(&snapshot.station, &station)
                            || !authorization.permits_resource(&snapshot.station)
                    }) {
                        return Err(outside_scope());
                    }
                    Ok(TargetQueryResult::StationSnapshot(snapshot))
                }
                TargetQuery::StationSnapshots(query) => {
                    // The trusted whitelist is applied in SQL before ORDER BY/LIMIT. Filtering a
                    // bounded page here would hide authorized stations behind forbidden rows.
                    let stations = authorization.station_resources().collect();
                    let page = self
                        .store
                        .read_scoped_snapshots(query, stations)
                        .await
                        .map_err(|error| storage_error(&error))?;
                    if page.items.iter().any(|item| {
                        !station_ref(&item.station)
                            || !authorization.permits_resource(&item.station)
                    }) {
                        return Err(outside_scope());
                    }
                    Ok(TargetQueryResult::StationSnapshots(page))
                }
                TargetQuery::CommandResult(request_id) => {
                    let result = self
                        .store
                        .command_result_by_request_id(request_id)
                        .await
                        .map_err(|error| storage_error(&error))?;
                    if result
                        .as_ref()
                        .is_some_and(|result| !authorization.permits_resource(&result.resource))
                    {
                        return Err(outside_scope());
                    }
                    Ok(TargetQueryResult::CommandResult(result))
                }
                TargetQuery::CommandHistory(query) => {
                    require_station(&query.station)?;
                    let scope = authorization.command_history_scope(&query.station);
                    if scope.is_empty() {
                        return Err(outside_scope());
                    }
                    let page = self
                        .store
                        .read_command_history(query.clone(), scope.clone())
                        .await
                        .map_err(|error| storage_error(&error))?;
                    if page.items.len() > usize::from(query.limit.get())
                        || page
                            .items
                            .iter()
                            .any(|item| !scope.permits(&item.resource, &query.station))
                    {
                        return Err(outside_scope());
                    }
                    Ok(TargetQueryResult::CommandHistory(page))
                }
                TargetQuery::RetainedEvents(query) => {
                    self.query_retained_events(authorization, query).await
                }
                TargetQuery::DataPointDescriptor { .. }
                | TargetQuery::DataPointValue { .. }
                | TargetQuery::Capabilities(_) => Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "query.operation_not_supported",
                )),
            }
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<Value>> {
        Box::pin(async move {
            require_grant(authorization, &query.resource)?;
            let resource = query.resource;
            let after = query.after;
            let store = self.store.clone();
            // A single event per read gives each delivered event its exact storage checkpoint.
            // The first read validates a supplied cursor before SSE headers are sent.
            let page = store
                .read_retained_events(RetainedEventQuery {
                    resource: resource.clone(),
                    after: after.clone(),
                    limit: one(),
                })
                .await
                .map_err(|error| storage_error(&error))?;
            let (cursor, pending) = checked_item(page, &resource, after.as_ref())?;
            Ok(Box::pin(RetainedSubscription {
                store,
                resource,
                cursor,
                pending,
                read: None,
                delay: None,
                stopped: false,
            }) as TargetRetainedEventStream<Value>)
        })
    }
}

struct RetainedSubscription {
    store: ManagementStore,
    resource: ResourceRef,
    cursor: Option<RetainedEventCursor>,
    pending: Option<RetainedEventItem<Value>>,
    read: Option<StorageFuture<'static, RetainedEventPage<StationEvent>>>,
    delay: Option<Pin<Box<Sleep>>>,
    stopped: bool,
}

impl TargetSubscription<Value> for RetainedSubscription {
    fn poll_event(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<RetainedEventItem<Value>, TargetPortError>>> {
        let this = self.get_mut();
        if this.stopped {
            return Poll::Ready(None);
        }
        if let Some(item) = this.pending.take() {
            return Poll::Ready(Some(Ok(item)));
        }
        if let Some(delay) = &mut this.delay {
            if delay.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            this.delay = None;
        }
        if this.read.is_none() {
            let store = this.store.clone();
            let query = RetainedEventQuery {
                resource: this.resource.clone(),
                after: this.cursor.clone(),
                limit: one(),
            };
            this.read = Some(Box::pin(
                async move { store.read_retained_events(query).await },
            ));
        }
        let Poll::Ready(result) = this
            .read
            .as_mut()
            .expect("read created above")
            .as_mut()
            .poll(cx)
        else {
            return Poll::Pending;
        };
        this.read = None;
        match result
            .map_err(|error| storage_error(&error))
            .and_then(|page| checked_item(page, &this.resource, this.cursor.as_ref()))
        {
            Ok((cursor, Some(item))) => {
                this.cursor = cursor;
                Poll::Ready(Some(Ok(item)))
            }
            Ok((cursor, None)) => {
                this.cursor = cursor;
                // At the live end, sleep rather than spinning on an instantly ready empty read.
                let mut delay = Box::pin(time::sleep(Duration::from_secs(1)));
                if delay.as_mut().poll(cx).is_ready() {
                    cx.waker().wake_by_ref();
                }
                this.delay = Some(delay);
                Poll::Pending
            }
            Err(error) => {
                this.stopped = true;
                Poll::Ready(Some(Err(error)))
            }
        }
    }

    fn capacity(&self) -> usize {
        1
    }
    fn backlog(&self) -> usize {
        usize::from(self.pending.is_some())
    }
}

fn checked_item(
    page: RetainedEventPage<StationEvent>,
    resource: &ResourceRef,
    after: Option<&RetainedEventCursor>,
) -> Result<
    (
        Option<RetainedEventCursor>,
        Option<RetainedEventItem<Value>>,
    ),
    TargetPortError,
> {
    if page.events.len() > 1 || (page.has_more && page.events.is_empty()) {
        return Err(invalid_result());
    }
    let cursor = page.resume_cursor;
    if let Some(event) = page.events.into_iter().next() {
        if !same_resource(&event.resource, resource) {
            return Err(outside_scope());
        }
        let checkpoint = cursor
            .clone()
            .filter(|next| after != Some(next))
            .ok_or_else(invalid_result)?;
        Ok((
            cursor,
            Some(RetainedEventItem {
                cursor: checkpoint,
                event: json_event(event)?,
            }),
        ))
    } else {
        if cursor.as_ref() != after {
            return Err(invalid_result());
        }
        Ok((cursor, None))
    }
}

fn json_event(event: EventEnvelope<StationEvent>) -> Result<EventEnvelope<Value>, TargetPortError> {
    let EventEnvelope {
        event_id,
        schema_version,
        runtime,
        resource,
        source_time,
        observed_at,
        event_type,
        origin,
        sequence,
        correlation_id,
        causation_id,
        provenance,
        payload,
    } = event;
    let payload = serde_json::to_value(payload).map_err(|_| invalid_result())?;
    Ok(EventEnvelope {
        event_id,
        schema_version,
        runtime,
        resource,
        source_time,
        observed_at,
        event_type,
        origin,
        sequence,
        correlation_id,
        causation_id,
        provenance,
        payload,
    })
}

fn one() -> PageLimit {
    PageLimit::new(1).expect("one is a valid storage page")
}
fn station_ref(resource: &ResourceRef) -> bool {
    resource.resource.is_none()
        && matches!(
            resource.native_protocol_reference,
            None | Some(
                NativeProtocolReference::Ocpp16 { connector_id: 0 }
                    | NativeProtocolReference::Ocpp201 {
                        evse_id: 0,
                        connector_id: None,
                    },
            )
        )
}
fn same_station(left: &ResourceRef, right: &ResourceRef) -> bool {
    left.bridge_id == right.bridge_id && left.station_id == right.station_id
}
fn same_resource(left: &ResourceRef, right: &ResourceRef) -> bool {
    same_station(left, right) && left.resource == right.resource
}
fn require_station(resource: &ResourceRef) -> Result<(), TargetPortError> {
    if station_ref(resource) {
        Ok(())
    } else {
        Err(TargetPortError::new(
            TargetPortErrorCode::InvalidRequest,
            "query.station_required",
        ))
    }
}
fn require_grant(
    authorization: &TargetQueryAuthorization,
    resource: &ResourceRef,
) -> Result<(), TargetPortError> {
    if authorization.permits_resource(resource) {
        Ok(())
    } else {
        Err(outside_scope())
    }
}
fn outside_scope() -> TargetPortError {
    TargetPortError::new(
        TargetPortErrorCode::Unauthorized,
        "query.result_outside_scope",
    )
}
fn invalid_result() -> TargetPortError {
    TargetPortError::new(
        TargetPortErrorCode::Unavailable,
        "query.source_invalid_result",
    )
}
fn storage_error(error: &StorageError) -> TargetPortError {
    let code = match error.code() {
        StorageErrorCode::InvalidRequest => TargetPortErrorCode::InvalidRequest,
        StorageErrorCode::CursorExpired => TargetPortErrorCode::CursorExpired,
        StorageErrorCode::Busy | StorageErrorCode::CapacityExhausted => TargetPortErrorCode::Busy,
        StorageErrorCode::Conflict
        | StorageErrorCode::Unavailable
        | StorageErrorCode::IntegrityFailure => TargetPortErrorCode::Unavailable,
    };
    TargetPortError::new(code, "query.storage_unavailable")
}

#[cfg(test)]
#[path = "management_source_tests.rs"]
mod tests;
