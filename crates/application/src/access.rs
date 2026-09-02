use std::sync::Arc;

use uob_contracts::{
    AuthenticatedCommandOrigin, CanonicalResource, CommandOperation, CommandResult,
    ExternalCommand, ResourceRef,
};

use crate::{
    CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
};

/// Coarse application surfaces granted to one authenticated transport principal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessPermission {
    /// Read canonical state, capabilities, command status, and retained events.
    Read,
    /// Submit ordinary charging commands such as start, stop, and charging limits.
    Control,
    /// Submit schema-pinned protocol management operations.
    PrivilegedControl,
}

/// Explicit canonical resource grant established by trusted credential configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccessResourceScope {
    /// Every station and child resource owned by one bridge installation.
    Bridge(uob_contracts::BridgeId),
    /// One station and all of its child resources.
    Station {
        /// Bridge installation owning the station.
        bridge_id: uob_contracts::BridgeId,
        /// Station whose descendants are granted.
        station_id: uob_contracts::StationId,
    },
    /// Exactly one canonical station, EVSE, or connector resource.
    Resource(ResourceRef),
}

impl AccessResourceScope {
    fn allows(&self, requested: &ResourceRef) -> bool {
        match self {
            Self::Bridge(bridge_id) => requested.bridge_id == *bridge_id,
            Self::Station {
                bridge_id,
                station_id,
            } => requested.bridge_id == *bridge_id && requested.station_id == *station_id,
            Self::Resource(granted) => same_canonical_resource(granted, requested),
        }
    }
}

/// Validated permissions and resources bound to one authenticated transport identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessGrant {
    origin: AuthenticatedCommandOrigin,
    permissions: Vec<AccessPermission>,
    resource_scopes: Vec<AccessResourceScope>,
}

impl AccessGrant {
    /// Validates an explicit grant without resolving credential material.
    ///
    /// # Errors
    ///
    /// Rejects credentials without a permission or canonical resource scope. Empty scopes are
    /// forbidden even for read-only credentials so adding control later cannot accidentally turn
    /// an unscoped observer into a global command publisher.
    pub fn new(
        origin: AuthenticatedCommandOrigin,
        permissions: Vec<AccessPermission>,
        resource_scopes: Vec<AccessResourceScope>,
    ) -> Result<Self, AccessPolicyError> {
        if permissions.is_empty() {
            return Err(AccessPolicyError::MissingPermission);
        }
        if resource_scopes.is_empty() {
            return Err(AccessPolicyError::MissingResourceScope);
        }
        Ok(Self {
            origin,
            permissions,
            resource_scopes,
        })
    }

    /// Returns the trusted command origin attached after transport authentication.
    #[must_use]
    pub const fn origin(&self) -> &AuthenticatedCommandOrigin {
        &self.origin
    }

    /// Returns whether this credential explicitly grants a permission on a canonical resource.
    #[must_use]
    pub fn permits(&self, permission: AccessPermission, resource: &ResourceRef) -> bool {
        self.permissions.contains(&permission)
            && self
                .resource_scopes
                .iter()
                .any(|scope| scope.allows(resource))
    }

    fn authorize_command<P>(&self, command: &ExternalCommand<P>) -> Result<(), AccessPolicyError> {
        if command.origin != self.origin {
            return Err(AccessPolicyError::OriginMismatch);
        }
        let permission = match command.request.operation {
            CommandOperation::Ocpp(_) => AccessPermission::PrivilegedControl,
            _ => AccessPermission::Control,
        };
        if !self.permissions.contains(&permission) {
            return Err(AccessPolicyError::PermissionDenied);
        }
        if !self
            .resource_scopes
            .iter()
            .any(|scope| scope.allows(&command.request.resource))
        {
            return Err(AccessPolicyError::ResourceDenied);
        }
        Ok(())
    }
}

/// Stable fail-closed access-policy rejections.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessPolicyError {
    /// A configured credential grants no operation class.
    MissingPermission,
    /// A configured credential has no canonical resource boundary.
    MissingResourceScope,
    /// No authenticated credential grant was configured for a command surface.
    MissingGrant,
    /// More than one credential grant claims the same authenticated origin.
    DuplicateOrigin,
    /// The request carries an origin other than the authenticated credential identity.
    OriginMismatch,
    /// The credential lacks the required read, control, or privileged-control permission.
    PermissionDenied,
    /// The requested canonical resource is outside the credential grant.
    ResourceDenied,
}

/// Complete immutable credential policy for one adapter command surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessPolicy {
    grants: Vec<AccessGrant>,
}

impl AccessPolicy {
    /// Validates a nonempty set of grants with unique authenticated origins.
    ///
    /// # Errors
    ///
    /// Rejects an empty policy or ambiguous duplicate origin.
    pub fn new(grants: Vec<AccessGrant>) -> Result<Self, AccessPolicyError> {
        if grants.is_empty() {
            return Err(AccessPolicyError::MissingGrant);
        }
        for (index, grant) in grants.iter().enumerate() {
            if grants[..index]
                .iter()
                .any(|existing| existing.origin == grant.origin)
            {
                return Err(AccessPolicyError::DuplicateOrigin);
            }
        }
        Ok(Self { grants })
    }

    /// Creates a policy for one already validated credential grant.
    #[must_use]
    pub fn single(grant: AccessGrant) -> Self {
        Self {
            grants: vec![grant],
        }
    }

    /// Returns the exact grant established for an authenticated origin.
    #[must_use]
    pub fn grant_for(&self, origin: &AuthenticatedCommandOrigin) -> Option<&AccessGrant> {
        self.grants.iter().find(|grant| grant.origin() == origin)
    }

    fn authorize_command<P>(&self, command: &ExternalCommand<P>) -> Result<(), AccessPolicyError> {
        self.grant_for(&command.origin)
            .ok_or(AccessPolicyError::OriginMismatch)?
            .authorize_command(command)
    }
}

/// Command ingress guard shared by management and target adapters.
///
/// This guard enforces the authenticated transport grant before calling the common application
/// command port. The inner application port must still reapply capability, safety, expiry,
/// idempotency, and application authorization policy.
pub struct ScopedCommandAdmissionPort<P> {
    inner: Arc<dyn CommandAdmissionPort<P>>,
    policy: AccessPolicy,
}

impl<P> ScopedCommandAdmissionPort<P> {
    /// Wraps the common application command path with an immutable credential grant.
    #[must_use]
    pub fn new(inner: Arc<dyn CommandAdmissionPort<P>>, policy: AccessPolicy) -> Self {
        Self { inner, policy }
    }

    /// Returns the transport grant enforced by this port.
    #[must_use]
    pub const fn policy(&self) -> &AccessPolicy {
        &self.policy
    }
}

impl<P: Send + 'static> CommandAdmissionPort<P> for ScopedCommandAdmissionPort<P> {
    fn submit(&self, command: ExternalCommand<P>) -> CommandAdmissionFuture<'_, CommandResult> {
        if let Err(error) = self.policy.authorize_command(&command) {
            let context = match error {
                AccessPolicyError::OriginMismatch => "access.command_origin_mismatch",
                AccessPolicyError::PermissionDenied => "access.command_permission_denied",
                AccessPolicyError::ResourceDenied => "access.command_resource_denied",
                AccessPolicyError::MissingPermission
                | AccessPolicyError::MissingResourceScope
                | AccessPolicyError::MissingGrant
                | AccessPolicyError::DuplicateOrigin => "access.invalid_grant",
            };
            return Box::pin(async move {
                Err(CommandAdmissionError::new(
                    CommandAdmissionErrorCode::Unauthorized,
                    context,
                ))
            });
        }
        self.inner.submit(command)
    }
}

fn same_canonical_resource(left: &ResourceRef, right: &ResourceRef) -> bool {
    left.bridge_id == right.bridge_id
        && left.station_id == right.station_id
        && same_resource_kind(left.resource.as_ref(), right.resource.as_ref())
}

fn same_resource_kind(left: Option<&CanonicalResource>, right: Option<&CanonicalResource>) -> bool {
    match (left, right) {
        (None, None) => true,
        (
            Some(CanonicalResource::Connector { connector_id: left }),
            Some(CanonicalResource::Connector {
                connector_id: right,
            }),
        ) => left == right,
        (
            Some(CanonicalResource::Evse {
                evse_id: left_evse,
                connector_id: left_connector,
            }),
            Some(CanonicalResource::Evse {
                evse_id: right_evse,
                connector_id: right_connector,
            }),
        ) => left_evse == right_evse && left_connector == right_connector,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll, Waker},
    };

    use uob_contracts::{
        AuthenticatedCommandOrigin, BridgeId, CommandOperation, CommandRequest, ExternalCommand,
        PrincipalId, RequestId, ResourceRef, StationId, UtcTimestamp,
    };

    use crate::{
        AccessGrant, AccessPermission, AccessPolicy, AccessPolicyError, AccessResourceScope,
        CommandAdmissionFuture, CommandAdmissionPort, ScopedCommandAdmissionPort,
    };

    struct CountingCommands(AtomicUsize);

    impl CommandAdmissionPort<()> for CountingCommands {
        fn submit(
            &self,
            _command: ExternalCommand<()>,
        ) -> CommandAdmissionFuture<'_, uob_contracts::CommandResult> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(crate::CommandAdmissionError::new(
                    crate::CommandAdmissionErrorCode::Unavailable,
                    "fixture",
                ))
            })
        }
    }

    fn resource(station: &str) -> ResourceRef {
        ResourceRef {
            bridge_id: BridgeId::new("bridge-a").unwrap(),
            station_id: StationId::new(station).unwrap(),
            resource: None,
            native_protocol_reference: None,
        }
    }

    fn origin() -> AuthenticatedCommandOrigin {
        AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new("operator-a").unwrap(),
        }
    }

    fn command(resource: ResourceRef) -> ExternalCommand<()> {
        ExternalCommand::authenticated(
            CommandRequest {
                request_id: RequestId::new("request-a").unwrap(),
                correlation_id: None,
                resource,
                operation: CommandOperation::Start {
                    authorization_reference: None,
                },
                expires_at: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
            },
            origin(),
        )
    }

    fn resolve<T>(
        mut future: CommandAdmissionFuture<'_, T>,
    ) -> Result<T, crate::CommandAdmissionError> {
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("fixture command future unexpectedly pending"),
        }
    }

    #[test]
    fn empty_permissions_and_resources_fail_configuration() {
        assert_eq!(
            AccessGrant::new(
                origin(),
                vec![],
                vec![AccessResourceScope::Resource(resource("a"))]
            ),
            Err(AccessPolicyError::MissingPermission)
        );
        assert_eq!(
            AccessGrant::new(origin(), vec![AccessPermission::Control], vec![]),
            Err(AccessPolicyError::MissingResourceScope)
        );
    }

    #[test]
    fn read_only_and_other_station_credentials_never_reach_application_commands() {
        let inner = Arc::new(CountingCommands(AtomicUsize::new(0)));
        let read_only = AccessGrant::new(
            origin(),
            vec![AccessPermission::Read],
            vec![AccessResourceScope::Station {
                bridge_id: BridgeId::new("bridge-a").unwrap(),
                station_id: StationId::new("station-a").unwrap(),
            }],
        )
        .unwrap();
        let read_port =
            ScopedCommandAdmissionPort::new(inner.clone(), AccessPolicy::single(read_only));
        assert_eq!(
            resolve(read_port.submit(command(resource("station-a"))))
                .unwrap_err()
                .code(),
            crate::CommandAdmissionErrorCode::Unauthorized
        );

        let station_a = AccessGrant::new(
            origin(),
            vec![AccessPermission::Control],
            vec![AccessResourceScope::Resource(resource("station-a"))],
        )
        .unwrap();
        let scoped_port =
            ScopedCommandAdmissionPort::new(inner.clone(), AccessPolicy::single(station_a));
        assert_eq!(
            resolve(scoped_port.submit(command(resource("station-b"))))
                .unwrap_err()
                .code(),
            crate::CommandAdmissionErrorCode::Unauthorized
        );
        assert_eq!(inner.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn authorized_transport_still_uses_application_admission() {
        let inner = Arc::new(CountingCommands(AtomicUsize::new(0)));
        let grant = AccessGrant::new(
            origin(),
            vec![AccessPermission::Control],
            vec![AccessResourceScope::Resource(resource("station-a"))],
        )
        .unwrap();
        let port = ScopedCommandAdmissionPort::new(inner.clone(), AccessPolicy::single(grant));
        assert_eq!(
            resolve(port.submit(command(resource("station-a"))))
                .unwrap_err()
                .code(),
            crate::CommandAdmissionErrorCode::Unavailable
        );
        assert_eq!(inner.0.load(Ordering::SeqCst), 1);
    }
}
