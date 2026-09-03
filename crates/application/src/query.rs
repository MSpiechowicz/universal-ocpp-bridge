use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use uob_contracts::{BridgeId, CanonicalResource, ResourceRef, StationId, TargetInstanceId};

use crate::{
    RetainedEventItem, RetainedEventQuery, TargetPortError, TargetPortErrorCode, TargetPortFuture,
    TargetQuery, TargetQueryPort, TargetQueryResult, TargetRetainedEventStream, TargetSubscription,
};

/// Canonical query classes granted to one configured target instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetQueryPermission {
    /// Current station snapshots and bounded station inventory.
    StationSnapshots,
    /// Canonical data-point descriptions and current values.
    DataPoints,
    /// Explicit resource capability descriptions.
    Capabilities,
    /// Durable command lifecycle status.
    CommandStatus,
    /// Paginated and streamed durable retained events.
    RetainedEvents,
}

/// One canonical resource grant established by trusted host configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetResourceScope {
    /// Every canonical resource below one station.
    Station {
        /// Bridge installation owning the station.
        bridge_id: BridgeId,
        /// Station whose descendants are granted.
        station_id: StationId,
    },
    /// Exactly one station, EVSE, or connector resource.
    Resource(ResourceRef),
}

impl TargetResourceScope {
    fn allows(&self, requested: &ResourceRef) -> bool {
        match self {
            Self::Station {
                bridge_id,
                station_id,
            } => requested.bridge_id == *bridge_id && requested.station_id == *station_id,
            Self::Resource(granted) => same_canonical_resource(granted, requested),
        }
    }
}

/// Trusted, immutable authorization context bound to one target's query port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetQueryAuthorization {
    target_instance_id: TargetInstanceId,
    permissions: Vec<TargetQueryPermission>,
    resource_scopes: Vec<TargetResourceScope>,
}

impl TargetQueryAuthorization {
    /// Binds explicit permissions and resource grants to a configured target instance.
    #[must_use]
    pub fn new(
        target_instance_id: TargetInstanceId,
        permissions: Vec<TargetQueryPermission>,
        resource_scopes: Vec<TargetResourceScope>,
    ) -> Self {
        Self {
            target_instance_id,
            permissions,
            resource_scopes,
        }
    }

    /// Returns the configured target instance to which this authorization belongs.
    #[must_use]
    pub const fn target_instance_id(&self) -> &TargetInstanceId {
        &self.target_instance_id
    }

    /// Returns whether a query class is explicitly granted.
    #[must_use]
    pub fn permits(&self, permission: TargetQueryPermission) -> bool {
        self.permissions.contains(&permission)
    }

    /// Returns whether a canonical resource is within an explicit target scope.
    #[must_use]
    pub fn permits_resource(&self, resource: &ResourceRef) -> bool {
        self.resource_scopes
            .iter()
            .any(|scope| scope.allows(resource))
    }
}

/// Application-owned canonical read source used behind a scoped target port.
///
/// Implementations may combine authoritative storage and current application state. The source
/// receives the trusted scope so station pages can be filtered before cursors are calculated.
/// Target adapters never receive this object or a concrete storage connection.
pub trait CanonicalQuerySource<E>: Send + Sync {
    /// Executes one canonical read under the supplied host authorization context.
    fn query<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<E>>;

    /// Opens one bounded retained-event stream under the supplied host context.
    fn subscribe_retained_events<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<E>>;
}

/// Concrete target-facing port that applies immutable scope checks around canonical reads.
pub struct ScopedTargetQueryPort<E> {
    source: Arc<dyn CanonicalQuerySource<E>>,
    authorization: TargetQueryAuthorization,
}

impl<E> ScopedTargetQueryPort<E> {
    /// Creates a target-facing port with host-established authorization.
    #[must_use]
    pub fn new(
        source: Arc<dyn CanonicalQuerySource<E>>,
        authorization: TargetQueryAuthorization,
    ) -> Self {
        Self {
            source,
            authorization,
        }
    }

    /// Returns the immutable host authorization bound to this port.
    #[must_use]
    pub const fn authorization(&self) -> &TargetQueryAuthorization {
        &self.authorization
    }
}

impl<E: Send + 'static> TargetQueryPort<E> for ScopedTargetQueryPort<E> {
    fn query(&self, query: TargetQuery) -> TargetPortFuture<'_, TargetQueryResult<E>> {
        let validation = validate_query(&self.authorization, &query);
        let source = Arc::clone(&self.source);
        let authorization = self.authorization.clone();

        Box::pin(async move {
            validation?;
            let expected = query.clone();
            let result = source.query(&authorization, query).await?;
            validate_result(&authorization, &expected, &result)?;
            Ok(result)
        })
    }

    fn subscribe_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'_, TargetRetainedEventStream<E>> {
        let validation = require_resource(
            &self.authorization,
            TargetQueryPermission::RetainedEvents,
            &query.resource,
        );
        let source = Arc::clone(&self.source);
        let authorization = self.authorization.clone();

        Box::pin(async move {
            validation?;
            let resource = query.resource.clone();
            let maximum_capacity = usize::from(query.limit.get());
            let inner = source
                .subscribe_retained_events(&authorization, query)
                .await?;
            if inner.capacity() == 0 || inner.capacity() > maximum_capacity {
                return Err(invalid("query.source_exceeded_subscription_limit"));
            }
            Ok(Box::pin(ScopedRetainedEventStream {
                inner,
                authorization,
                resource,
                stopped: false,
            }) as TargetRetainedEventStream<E>)
        })
    }
}

struct ScopedRetainedEventStream<E> {
    inner: TargetRetainedEventStream<E>,
    authorization: TargetQueryAuthorization,
    resource: ResourceRef,
    stopped: bool,
}

impl<E: Send> TargetSubscription<E> for ScopedRetainedEventStream<E> {
    fn poll_event(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<RetainedEventItem<E>, TargetPortError>>> {
        let this = self.get_mut();
        if this.stopped {
            return Poll::Ready(None);
        }

        match this.inner.as_mut().poll_event(context) {
            Poll::Ready(Some(Ok(item)))
                if this.authorization.permits_resource(&item.event.resource)
                    && same_canonical_resource(&this.resource, &item.event.resource) =>
            {
                Poll::Ready(Some(Ok(item)))
            }
            Poll::Ready(Some(Ok(_))) => {
                this.stopped = true;
                Poll::Ready(Some(Err(unauthorized("query.event_outside_scope"))))
            }
            other => other,
        }
    }

    fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    fn backlog(&self) -> usize {
        self.inner.backlog()
    }
}

fn validate_query(
    authorization: &TargetQueryAuthorization,
    query: &TargetQuery,
) -> Result<(), TargetPortError> {
    match query {
        TargetQuery::StationSnapshot(resource) => {
            if resource.resource.is_some() {
                return Err(invalid("query.station_snapshot_requires_station"));
            }
            require_resource(
                authorization,
                TargetQueryPermission::StationSnapshots,
                resource,
            )
        }
        TargetQuery::StationSnapshots(_) => {
            require_permission(authorization, TargetQueryPermission::StationSnapshots)?;
            if authorization.resource_scopes.is_empty() {
                return Err(unauthorized("query.no_resource_scope"));
            }
            Ok(())
        }
        TargetQuery::DataPointDescriptor { resource, .. }
        | TargetQuery::DataPointValue { resource, .. } => {
            require_resource(authorization, TargetQueryPermission::DataPoints, resource)
        }
        TargetQuery::Capabilities(resource) => {
            require_resource(authorization, TargetQueryPermission::Capabilities, resource)
        }
        TargetQuery::CommandResult(_) => {
            require_permission(authorization, TargetQueryPermission::CommandStatus)
        }
        TargetQuery::RetainedEvents(query) => require_resource(
            authorization,
            TargetQueryPermission::RetainedEvents,
            &query.resource,
        ),
    }
}

fn validate_result<E>(
    authorization: &TargetQueryAuthorization,
    query: &TargetQuery,
    result: &TargetQueryResult<E>,
) -> Result<(), TargetPortError> {
    match (query, result) {
        (TargetQuery::StationSnapshot(requested), TargetQueryResult::StationSnapshot(snapshot)) => {
            if let Some(snapshot) = snapshot {
                require_matching_resource(authorization, requested, &snapshot.station)?;
            }
        }
        (TargetQuery::StationSnapshots(query), TargetQueryResult::StationSnapshots(page)) => {
            require_page_bound(query.limit.get(), page.items.len())?;
            for snapshot in &page.items {
                if snapshot.station.resource.is_some()
                    || !authorization.permits_resource(&snapshot.station)
                {
                    return Err(unauthorized("query.snapshot_page_outside_scope"));
                }
            }
        }
        (
            TargetQuery::DataPointDescriptor { resource, point_id },
            TargetQueryResult::DataPointDescriptor(descriptor),
        ) => {
            if let Some(descriptor) = descriptor {
                require_matching_resource(authorization, resource, &descriptor.resource)?;
                if descriptor.point_id != *point_id {
                    return Err(invalid("query.point_descriptor_mismatch"));
                }
            }
        }
        (
            TargetQuery::DataPointValue { point_id, .. },
            TargetQueryResult::DataPointValue(value),
        ) => {
            if value
                .as_ref()
                .is_some_and(|value| value.point_id != *point_id)
            {
                return Err(invalid("query.point_value_mismatch"));
            }
        }
        (TargetQuery::Capabilities(_), TargetQueryResult::Capabilities(_)) => {}
        (TargetQuery::CommandResult(request_id), TargetQueryResult::CommandResult(result)) => {
            if let Some(result) = result {
                if result.return_route.request_id != *request_id {
                    return Err(invalid("query.command_result_mismatch"));
                }
                if !authorization.permits_resource(&result.resource) {
                    return Err(unauthorized("query.command_result_outside_scope"));
                }
            }
        }
        (TargetQuery::RetainedEvents(query), TargetQueryResult::RetainedEvents(page)) => {
            require_page_bound(query.limit.get(), page.items.len())?;
            for event in &page.items {
                require_matching_resource(authorization, &query.resource, &event.resource)?;
            }
        }
        _ => return Err(invalid("query.response_type_mismatch")),
    }
    Ok(())
}

fn require_permission(
    authorization: &TargetQueryAuthorization,
    permission: TargetQueryPermission,
) -> Result<(), TargetPortError> {
    if authorization.permits(permission) {
        Ok(())
    } else {
        Err(TargetPortError::new(
            TargetPortErrorCode::Unsupported,
            "query.operation_not_granted",
        ))
    }
}

fn require_resource(
    authorization: &TargetQueryAuthorization,
    permission: TargetQueryPermission,
    resource: &ResourceRef,
) -> Result<(), TargetPortError> {
    require_permission(authorization, permission)?;
    if authorization.permits_resource(resource) {
        Ok(())
    } else {
        Err(unauthorized("query.resource_outside_scope"))
    }
}

fn require_matching_resource(
    authorization: &TargetQueryAuthorization,
    requested: &ResourceRef,
    returned: &ResourceRef,
) -> Result<(), TargetPortError> {
    if authorization.permits_resource(returned) && same_canonical_resource(requested, returned) {
        Ok(())
    } else {
        Err(unauthorized("query.result_outside_scope"))
    }
}

fn require_page_bound(limit: u16, actual: usize) -> Result<(), TargetPortError> {
    if actual <= usize::from(limit) {
        Ok(())
    } else {
        Err(invalid("query.source_exceeded_page_limit"))
    }
}

fn same_canonical_resource(left: &ResourceRef, right: &ResourceRef) -> bool {
    left.bridge_id == right.bridge_id
        && left.station_id == right.station_id
        && same_resource_part(left.resource.as_ref(), right.resource.as_ref())
}

fn same_resource_part(left: Option<&CanonicalResource>, right: Option<&CanonicalResource>) -> bool {
    left == right
}

fn unauthorized(context: &'static str) -> TargetPortError {
    TargetPortError::new(TargetPortErrorCode::Unauthorized, context)
}

fn invalid(context: &'static str) -> TargetPortError {
    TargetPortError::new(TargetPortErrorCode::InvalidRequest, context)
}
