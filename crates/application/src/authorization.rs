use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, RwLock},
};

use uob_contracts::{
    CanonicalResource, CommandOperation, CommandResult, ExternalCommand, ResourceRef, UtcTimestamp,
};

use crate::{
    AtomicStoreWrite, AuthorizationChange, AuthorizationReference, AuthorizationState,
    CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
    OperationalStore, PageLimit, RecoveryQuery, StorageError,
};

/// Presented charging identity. Its bytes are deliberately omitted from `Debug` and `Display`.
pub struct SensitiveAuthorizationToken(Vec<u8>);

impl SensitiveAuthorizationToken {
    /// Copies a nonempty token into protected application memory.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorizationProviderError::InvalidToken`] for an empty token.
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, AuthorizationProviderError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(AuthorizationProviderError::InvalidToken);
        }
        Ok(Self(value.to_vec()))
    }

    /// Exposes token bytes only to an authorization provider for one resolution operation.
    #[must_use]
    pub fn expose_to_provider(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SensitiveAuthorizationToken {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Static provider facts validated by the trusted composition root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizationProviderDescriptor {
    /// Stable implementation kind.
    pub kind: &'static str,
    /// Whether this provider is restricted to staging and demo environments.
    pub test_only: bool,
}

/// Sanitized provider failures that never carry token material.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationProviderError {
    /// The presented token is structurally invalid.
    InvalidToken,
    /// The selected provider cannot currently resolve identities.
    Unavailable,
}

impl fmt::Display for AuthorizationProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken => formatter.write_str("authorization token is invalid"),
            Self::Unavailable => formatter.write_str("authorization provider is unavailable"),
        }
    }
}

impl Error for AuthorizationProviderError {}

/// Object-safe future returned by authorization providers.
pub type AuthorizationProviderFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<AuthorizationReference, AuthorizationProviderError>> + Send + 'a,
    >,
>;

/// Provider boundary that turns sensitive input into an opaque local reference.
pub trait AuthorizationProvider: Send + Sync {
    /// Describes the provider without exposing configuration or credentials.
    fn descriptor(&self) -> AuthorizationProviderDescriptor;

    /// Resolves a presented token. Implementations must not retain or report its bytes.
    fn resolve<'a>(
        &'a self,
        token: &'a SensitiveAuthorizationToken,
    ) -> AuthorizationProviderFuture<'a>;
}

/// Explicit local decision returned to OCPP and command admission callers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationDecision {
    /// The opaque reference is active for the requested charging resource.
    Allowed { reference: AuthorizationReference },
    /// The request failed closed for a stable reason.
    Denied { reason: AuthorizationDenialReason },
}

/// Stable denial reasons safe for protocol mapping and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationDenialReason {
    Unknown,
    Revoked,
    Expired,
    ResourceDenied,
    ProviderUnavailable,
}

/// Invalid or ambiguous persisted authorization policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationPolicyError {
    ConflictingRevision,
    StaleRevision,
}

/// In-memory decision index restored exclusively from authoritative local persistence.
#[derive(Clone, Default)]
pub struct LocalAuthorizationPolicy {
    entries: BTreeMap<String, AuthorizationChange>,
}

impl LocalAuthorizationPolicy {
    /// Restores the latest unambiguous revision for every opaque reference.
    ///
    /// # Errors
    ///
    /// Returns an error when duplicate references contain stale or conflicting revisions.
    pub fn restore(
        changes: impl IntoIterator<Item = AuthorizationChange>,
    ) -> Result<Self, AuthorizationPolicyError> {
        let mut policy = Self::default();
        for change in changes {
            policy.apply(change)?;
        }
        Ok(policy)
    }

    /// Applies a change only after its owning storage transaction committed.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale revision or a different change reusing the latest revision.
    pub fn apply(&mut self, change: AuthorizationChange) -> Result<(), AuthorizationPolicyError> {
        match self.entries.get(change.reference.as_str()) {
            Some(current) if current.revision > change.revision => {
                return Err(AuthorizationPolicyError::StaleRevision);
            }
            Some(current) if current.revision == change.revision && current != &change => {
                return Err(AuthorizationPolicyError::ConflictingRevision);
            }
            _ => {}
        }
        self.entries
            .insert(change.reference.as_str().to_owned(), change);
        Ok(())
    }

    /// Decides locally without consulting a target, browser, payment system, or network service.
    #[must_use]
    pub fn decide(
        &self,
        reference: &AuthorizationReference,
        resource: &ResourceRef,
        now: UtcTimestamp,
    ) -> AuthorizationDecision {
        let Some(entry) = self.entries.get(reference.as_str()) else {
            return denied(AuthorizationDenialReason::Unknown);
        };
        if matches!(entry.state, AuthorizationState::Revoked) {
            return denied(AuthorizationDenialReason::Revoked);
        }
        if entry.expires_at.is_some_and(|expires_at| now >= expires_at) {
            return denied(AuthorizationDenialReason::Expired);
        }
        if !resource_scope_allows(&entry.resource, resource) {
            return denied(AuthorizationDenialReason::ResourceDenied);
        }
        AuthorizationDecision::Allowed {
            reference: reference.clone(),
        }
    }
}

/// Durable local authorization service shared by station and command ingress.
pub struct LocalAuthorizationService<C, E, D, R> {
    store: Arc<dyn OperationalStore<C, E, D, R>>,
    policy: RwLock<LocalAuthorizationPolicy>,
}

impl<C, E, D, R> LocalAuthorizationService<C, E, D, R>
where
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    /// Restores the local decision index from the authoritative store after restart.
    ///
    /// # Errors
    ///
    /// Returns a sanitized storage error when recovery fails or persisted revisions conflict.
    pub async fn recover(
        store: Arc<dyn OperationalStore<C, E, D, R>>,
        limit: PageLimit,
    ) -> Result<Self, StorageError> {
        let recovered = store.recover(RecoveryQuery { limit }).await?;
        let policy = LocalAuthorizationPolicy::restore(recovered.authorization).map_err(|_| {
            StorageError::new(
                crate::StorageErrorCode::IntegrityFailure,
                "persisted authorization revisions conflict",
            )
        })?;
        Ok(Self {
            store,
            policy: RwLock::new(policy),
        })
    }

    /// Persists one policy mutation before making it visible to local decisions.
    ///
    /// # Errors
    ///
    /// Returns a storage conflict for a stale or ambiguous revision, or the underlying sanitized
    /// storage error when the atomic commit fails.
    pub async fn apply_change(&self, change: AuthorizationChange) -> Result<(), StorageError> {
        let mut candidate = self
            .policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        candidate.apply(change.clone()).map_err(|_| {
            StorageError::new(
                crate::StorageErrorCode::Conflict,
                "authorization revision conflicts",
            )
        })?;
        let mut write = AtomicStoreWrite::empty();
        write.authorization_changes.push(change.clone());
        self.store.write_atomic(write).await?;
        self.policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply(change)
            .map_err(|_| {
                StorageError::new(
                    crate::StorageErrorCode::Conflict,
                    "authorization revision conflicts",
                )
            })
    }

    /// Applies the same local policy to a station identity resolved to an opaque reference.
    #[must_use]
    pub fn authorize_reference(
        &self,
        reference: &AuthorizationReference,
        resource: &ResourceRef,
        now: UtcTimestamp,
    ) -> AuthorizationDecision {
        self.policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .decide(reference, resource, now)
    }

    /// Resolves a sensitive token and then applies the persisted local policy.
    pub async fn authorize_token(
        &self,
        provider: &dyn AuthorizationProvider,
        token: &SensitiveAuthorizationToken,
        resource: &ResourceRef,
        now: UtcTimestamp,
    ) -> AuthorizationDecision {
        match provider.resolve(token).await {
            Ok(reference) => self.authorize_reference(&reference, resource, now),
            Err(_) => denied(AuthorizationDenialReason::ProviderUnavailable),
        }
    }
}

/// Command guard that prevents a payload-supplied start reference from bypassing local policy.
pub struct AuthorizationGuardedCommandPort<C, E, D, R> {
    inner: Arc<dyn CommandAdmissionPort<C>>,
    authorization: Arc<LocalAuthorizationService<C, E, D, R>>,
    now: Arc<dyn Fn() -> UtcTimestamp + Send + Sync>,
}

impl<C, E, D, R> AuthorizationGuardedCommandPort<C, E, D, R> {
    #[must_use]
    pub fn new(
        inner: Arc<dyn CommandAdmissionPort<C>>,
        authorization: Arc<LocalAuthorizationService<C, E, D, R>>,
        now: Arc<dyn Fn() -> UtcTimestamp + Send + Sync>,
    ) -> Self {
        Self {
            inner,
            authorization,
            now,
        }
    }
}

impl<C, E, D, R> CommandAdmissionPort<C> for AuthorizationGuardedCommandPort<C, E, D, R>
where
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    fn submit(&self, command: ExternalCommand<C>) -> CommandAdmissionFuture<'_, CommandResult> {
        if let CommandOperation::Start {
            authorization_reference: Some(reference),
        } = &command.request.operation
        {
            let Ok(reference) = AuthorizationReference::new(reference.clone()) else {
                return rejected_command("authorization.reference_invalid");
            };
            if !matches!(
                self.authorization.authorize_reference(
                    &reference,
                    &command.request.resource,
                    (self.now)(),
                ),
                AuthorizationDecision::Allowed { .. }
            ) {
                return rejected_command("authorization.local_policy_denied");
            }
        }
        self.inner.submit(command)
    }
}

fn rejected_command(context: &'static str) -> CommandAdmissionFuture<'static, CommandResult> {
    Box::pin(async move {
        Err(CommandAdmissionError::new(
            CommandAdmissionErrorCode::PolicyRejected,
            context,
        ))
    })
}

fn denied(reason: AuthorizationDenialReason) -> AuthorizationDecision {
    AuthorizationDecision::Denied { reason }
}

fn resource_scope_allows(granted: &ResourceRef, requested: &ResourceRef) -> bool {
    if granted.bridge_id != requested.bridge_id || granted.station_id != requested.station_id {
        return false;
    }
    match (&granted.resource, &requested.resource) {
        (None, _) => true,
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
        ) => {
            left_evse == right_evse
                && (left_connector.is_none() || left_connector == right_connector)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
    use uob_contracts::{BridgeId, ResourceRef, StationId, UtcTimestamp};

    use super::{
        AuthorizationDecision, AuthorizationDenialReason, AuthorizationPolicyError,
        LocalAuthorizationPolicy,
    };
    use crate::{AuthorizationChange, AuthorizationReference, AuthorizationState};

    #[test]
    fn allowed_denied_expired_and_revoked_references_fail_closed() {
        let active = change("active", AuthorizationState::Active, 1, Some(timestamp(5)));
        let revoked = change("revoked", AuthorizationState::Revoked, 1, None);
        let policy = LocalAuthorizationPolicy::restore([active, revoked]).expect("valid policy");

        assert!(matches!(
            policy.decide(&reference("active"), &resource("station-a"), timestamp(4)),
            AuthorizationDecision::Allowed { .. }
        ));
        assert_eq!(
            policy.decide(&reference("missing"), &resource("station-a"), timestamp(4)),
            denied(AuthorizationDenialReason::Unknown)
        );
        assert_eq!(
            policy.decide(&reference("active"), &resource("station-b"), timestamp(4)),
            denied(AuthorizationDenialReason::ResourceDenied)
        );
        assert_eq!(
            policy.decide(&reference("active"), &resource("station-a"), timestamp(5)),
            denied(AuthorizationDenialReason::Expired)
        );
        assert_eq!(
            policy.decide(&reference("revoked"), &resource("station-a"), timestamp(4)),
            denied(AuthorizationDenialReason::Revoked)
        );
    }

    #[test]
    fn stale_or_ambiguous_policy_updates_are_rejected() {
        let mut policy = LocalAuthorizationPolicy::restore([change(
            "card",
            AuthorizationState::Active,
            2,
            None,
        )])
        .expect("policy");
        assert_eq!(
            policy.apply(change("card", AuthorizationState::Revoked, 1, None)),
            Err(AuthorizationPolicyError::StaleRevision)
        );
        assert_eq!(
            policy.apply(change("card", AuthorizationState::Revoked, 2, None)),
            Err(AuthorizationPolicyError::ConflictingRevision)
        );
    }

    fn denied(reason: AuthorizationDenialReason) -> AuthorizationDecision {
        AuthorizationDecision::Denied { reason }
    }

    fn change(
        value: &str,
        state: AuthorizationState,
        revision: u64,
        expires_at: Option<UtcTimestamp>,
    ) -> AuthorizationChange {
        AuthorizationChange {
            reference: reference(value),
            resource: resource("station-a"),
            state,
            revision,
            changed_at: timestamp(0),
            expires_at,
        }
    }

    fn reference(value: &str) -> AuthorizationReference {
        AuthorizationReference::new(value).expect("reference")
    }

    fn resource(station: &str) -> ResourceRef {
        ResourceRef {
            bridge_id: BridgeId::new("bridge-test").expect("bridge"),
            station_id: StationId::new(station).expect("station"),
            resource: None,
            native_protocol_reference: None,
        }
    }

    fn timestamp(minute: u8) -> UtcTimestamp {
        UtcTimestamp::new(
            PrimitiveDateTime::new(
                Date::from_calendar_date(2026, Month::September, 1).expect("date"),
                Time::from_hms(12, minute, 0).expect("time"),
            )
            .assume_offset(UtcOffset::UTC),
        )
    }
}
