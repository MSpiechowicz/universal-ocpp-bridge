use serde_json::Value;
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::mpsc;
use uob_application::{
    CanonicalQuerySource, OperationalStore, Page, RetainedEventItem, RetainedEventQuery,
    TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery, TargetQueryAuthorization,
    TargetQueryResult, TargetRetainedEventStream, TargetSubscription,
};
use uob_contracts::{ResourceRef, StationSnapshot, TransactionSnapshot};

pub type Store = uob_storage_adapter::SqliteOperationalStore<
    Value,
    TransactionSnapshot,
    TransactionSnapshot,
    String,
>;

pub struct Source(pub Store);
fn error(_: uob_application::StorageError) -> TargetPortError {
    TargetPortError::new(
        TargetPortErrorCode::Unavailable,
        "storage.query_unavailable",
    )
}
fn expired() -> TargetPortError {
    TargetPortError::new(TargetPortErrorCode::CursorExpired, "storage.cursor_expired")
}
fn resource_matches(actual: &ResourceRef, desired: &ResourceRef) -> bool {
    actual.bridge_id == desired.bridge_id
        && actual.station_id == desired.station_id
        && (desired.resource.is_none() || actual.resource == desired.resource)
}
async fn snapshot(
    store: &Store,
    resource: &ResourceRef,
) -> Result<Option<StationSnapshot>, TargetPortError> {
    let page = store
        .read_snapshots(uob_application::SnapshotQuery {
            after: None,
            limit: uob_application::PageLimit::new(100).unwrap(),
        })
        .await
        .map_err(error)?;
    Ok(page.items.into_iter().find(|snapshot| {
        snapshot.station.bridge_id == resource.bridge_id
            && snapshot.station.station_id == resource.station_id
    }))
}
impl CanonicalQuerySource<TransactionSnapshot> for Source {
    fn query<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<TransactionSnapshot>> {
        Box::pin(async move {
            Ok(match query {
                TargetQuery::StationSnapshot(resource) => {
                    TargetQueryResult::StationSnapshot(snapshot(&self.0, &resource).await?)
                }
                TargetQuery::StationSnapshots(query) => {
                    let mut page = self.0.read_snapshots(query).await.map_err(error)?;
                    page.items
                        .retain(|item| authorization.permits_resource(&item.station));
                    TargetQueryResult::StationSnapshots(page)
                }
                TargetQuery::CommandResult(id) => TargetQueryResult::CommandResult(
                    self.0
                        .command_result_by_request_id(id)
                        .await
                        .map_err(error)?,
                ),
                TargetQuery::Capabilities(resource) => TargetQueryResult::Capabilities(
                    snapshot(&self.0, &resource).await?.and_then(|snapshot| {
                        if resource.resource.is_none() {
                            Some(snapshot.capabilities)
                        } else {
                            snapshot
                                .resources
                                .into_iter()
                                .find(|item| resource_matches(&item.resource, &resource))
                                .map(|item| item.capabilities)
                        }
                    }),
                ),
                TargetQuery::DataPointDescriptor { resource, point_id } => {
                    let descriptor = snapshot(&self.0, &resource).await?.and_then(|snapshot| {
                        let descriptors = if resource.resource.is_none() {
                            Vec::new()
                        } else {
                            snapshot
                                .resources
                                .into_iter()
                                .find(|item| resource_matches(&item.resource, &resource))
                                .map_or_else(Vec::new, |item| item.data_points)
                        };
                        descriptors
                            .into_iter()
                            .find(|item| item.point_id == point_id)
                    });
                    TargetQueryResult::DataPointDescriptor(descriptor)
                }
                TargetQuery::DataPointValue { resource, point_id } => {
                    let value = snapshot(&self.0, &resource).await?.and_then(|snapshot| {
                        let values = if resource.resource.is_none() {
                            snapshot.current_values
                        } else {
                            snapshot
                                .resources
                                .into_iter()
                                .find(|item| resource_matches(&item.resource, &resource))
                                .map_or_else(Vec::new, |item| item.current_values)
                        };
                        values.into_iter().find(|item| item.point_id == point_id)
                    });
                    TargetQueryResult::DataPointValue(value)
                }
                TargetQuery::RetainedEvents(query) => {
                    let page = self.0.read_retained_events(query).await.map_err(error)?;
                    TargetQueryResult::RetainedEvents(Page {
                        items: page.events,
                        next_cursor: page.resume_cursor,
                    })
                }
            })
        })
    }
    fn subscribe_retained_events<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        mut query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<TransactionSnapshot>> {
        Box::pin(async move {
            let requested = query.resource.clone();
            if requested.resource.is_some() {
                let state = snapshot(&self.0, &requested).await?.ok_or_else(|| {
                    TargetPortError::new(TargetPortErrorCode::Unavailable, "station missing")
                })?;
                query.resource = state
                    .resources
                    .into_iter()
                    .find(|item| resource_matches(&item.resource, &requested))
                    .ok_or_else(|| {
                        TargetPortError::new(TargetPortErrorCode::Unavailable, "resource missing")
                    })?
                    .resource;
            }
            // Check the cursor before returning 200, so expiry stays an HTTP 410.
            self.0
                .read_retained_events(query.clone())
                .await
                .map_err(|error| {
                    if error.code() == uob_application::StorageErrorCode::CursorExpired {
                        expired()
                    } else {
                        self::error(error)
                    }
                })?;
            let (sender, receiver) = mpsc::channel(1);
            let store = self.0.clone();
            tokio::spawn(async move {
                let mut after = query.after;
                loop {
                    let page = store
                        .read_retained_events(RetainedEventQuery {
                            after: after.clone(),
                            limit: uob_application::PageLimit::new(1).unwrap(),
                            resource: query.resource.clone(),
                        })
                        .await;
                    match page {
                        Ok(page) if !page.events.is_empty() => {
                            let cursor = page.resume_cursor.expect("event checkpoint");
                            for mut event in page.events {
                                // HTTP resource selectors name canonical identity, not its native address.
                                event.resource = requested.clone();
                                if sender
                                    .send(Ok(RetainedEventItem {
                                        cursor: cursor.clone(),
                                        event,
                                    }))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            after = Some(cursor);
                        }
                        Ok(_) => tokio::select! {
                            () = sender.closed() => return,
                            () = tokio::time::sleep(Duration::from_millis(50)) => (),
                        },
                        Err(err) => {
                            let _ = sender
                                .send(Err(
                                    if err.code()
                                        == uob_application::StorageErrorCode::CursorExpired
                                    {
                                        expired()
                                    } else {
                                        self::error(err)
                                    },
                                ))
                                .await;
                            return;
                        }
                    }
                }
            });
            Ok(Box::pin(Subscription(receiver)) as TargetRetainedEventStream<TransactionSnapshot>)
        })
    }
}
struct Subscription(
    mpsc::Receiver<Result<RetainedEventItem<TransactionSnapshot>, TargetPortError>>,
);
impl TargetSubscription<TransactionSnapshot> for Subscription {
    fn poll_event(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<RetainedEventItem<TransactionSnapshot>, TargetPortError>>> {
        self.get_mut().0.poll_recv(cx)
    }
    fn capacity(&self) -> usize {
        1
    }
    fn backlog(&self) -> usize {
        self.0.len()
    }
}
