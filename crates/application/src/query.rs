mod validation;

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use validation::{
    invalid, require_resource, same_canonical_resource, unauthorized, validate_query,
    validate_result,
};

use uob_contracts::{BridgeId, NativeProtocolReference, ResourceRef, StationId, TargetInstanceId};

use crate::{
    CommandHistoryScope, RetainedEventItem, RetainedEventQuery, TargetPortError, TargetPortFuture,
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

    /// Station-only grants for SQL-filtered inventory reads. Child-resource grants
    /// never authorize a station snapshot containing unrelated sibling resources.
    pub fn station_resources(&self) -> impl Iterator<Item = ResourceRef> + '_ {
        self.resource_scopes.iter().filter_map(|scope| match scope {
            TargetResourceScope::Station {
                bridge_id,
                station_id,
            } => Some(ResourceRef {
                bridge_id: bridge_id.clone(),
                station_id: station_id.clone(),
                resource: None,
                native_protocol_reference: None,
            }),
            TargetResourceScope::Resource(resource)
                if resource.resource.is_none()
                    && matches!(
                        resource.native_protocol_reference,
                        None | Some(
                            NativeProtocolReference::Ocpp16 { connector_id: 0 }
                                | NativeProtocolReference::Ocpp201 {
                                    evse_id: 0,
                                    connector_id: None,
                                },
                        )
                    ) =>
            {
                Some(ResourceRef {
                    native_protocol_reference: None,
                    ..resource.clone()
                })
            }
            TargetResourceScope::Resource(_) => None,
        })
    }

    /// Builds the exact station/child predicate to apply before command-history pagination.
    #[must_use]
    pub fn command_history_scope(&self, station: &ResourceRef) -> CommandHistoryScope {
        let mut scope = CommandHistoryScope {
            descendants: false,
            station_only: false,
            resources: Vec::new(),
        };
        for grant in &self.resource_scopes {
            match grant {
                TargetResourceScope::Station {
                    bridge_id,
                    station_id,
                } if *bridge_id == station.bridge_id && *station_id == station.station_id => {
                    scope.descendants = true;
                }
                TargetResourceScope::Resource(resource)
                    if resource.bridge_id == station.bridge_id
                        && resource.station_id == station.station_id =>
                {
                    match &resource.resource {
                        Some(child) if !scope.resources.contains(child) => {
                            scope.resources.push(child.clone());
                        }
                        None if matches!(
                            resource.native_protocol_reference,
                            None | Some(
                                NativeProtocolReference::Ocpp16 { connector_id: 0 }
                                    | NativeProtocolReference::Ocpp201 {
                                        evse_id: 0,
                                        connector_id: None
                                    }
                            )
                        ) =>
                        {
                            scope.station_only = true;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        scope
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
