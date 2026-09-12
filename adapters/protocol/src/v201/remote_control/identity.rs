use super::RemoteStartIdentity;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uob_application::charging_identity::{
    ChargingIdentityProvider, ChargingIdentityResolution, PresentedChargingIdentity,
};
use uob_application::{
    AuthorizationDecision, AuthorizationReference, LocalAuthorizationService, StationCommandError,
};
use uob_contracts::{ResourceRef, UtcTimestamp};

/// Startup-resolved typed identities. No token material is serialized into durable commands.
pub struct LocalRemoteStartIdentity<C, E, D, R> {
    tokens: BTreeMap<String, PresentedChargingIdentity>,
    policy: Arc<LocalAuthorizationService<C, E, D, R>>,
}
impl<C, E, D, R> LocalRemoteStartIdentity<C, E, D, R> {
    /// Resolves at most 128 bounded identities through the configured trusted provider at startup.
    /// # Errors
    /// Rejects malformed, duplicate or unresolved tokens and provider timeouts without disclosure.
    pub async fn new(
        tokens: Vec<PresentedChargingIdentity>,
        provider: &dyn ChargingIdentityProvider,
        policy: Arc<LocalAuthorizationService<C, E, D, R>>,
    ) -> Result<Self, StationCommandError> {
        let invalid = || StationCommandError::new("invalid remote start identity configuration");
        if tokens.len() > 128 {
            return Err(invalid());
        }
        let mut entries = BTreeMap::new();
        for token in tokens {
            super::mapping::token_value(&token).map_err(|_| invalid())?;
            let resolution = tokio::time::timeout(Duration::from_secs(1), provider.resolve(&token))
                .await
                .map_err(|_| invalid())?
                .map_err(|_| invalid())?;
            let ChargingIdentityResolution::Resolved { reference, .. } = resolution else {
                return Err(invalid());
            };
            if entries
                .insert(reference.as_str().to_owned(), token)
                .is_some()
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            tokens: entries,
            policy,
        })
    }
}
impl<C: Send + 'static, E: Send + 'static, D: Send + 'static, R: Send + 'static> RemoteStartIdentity
    for LocalRemoteStartIdentity<C, E, D, R>
{
    fn authorized_token(
        &self,
        reference: &str,
        resource: &ResourceRef,
        now: UtcTimestamp,
    ) -> Option<PresentedChargingIdentity> {
        let token = self.tokens.get(reference)?;
        let reference = AuthorizationReference::new(reference.to_owned()).ok()?;
        matches!(
            self.policy.authorize_reference(&reference, resource, now),
            AuthorizationDecision::Allowed { .. }
        )
        .then(|| token.clone())
    }
}
