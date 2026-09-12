//! Bounded in-memory token resolution against the persisted local allowlist.
use super::RemoteStartIdentity;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
use uob_application::{
    AuthorizationDecision, AuthorizationReference, LocalAuthorizationService,
    SensitiveAuthorizationToken, StationCommandError,
};
use uob_contracts::{ResourceRef, UtcTimestamp};

/// Locally supplied token material, independently scoped by current durable authorization policy.
/// Secrets are not serializable or debug-printable and are zeroed by their owning token on drop.
/// The composition root resolves these secrets at startup; commands carry SHA-256 references only.
pub struct LocalRemoteStartIdentity<C, E, D, R> {
    tokens: BTreeMap<String, SensitiveAuthorizationToken>,
    policy: Arc<LocalAuthorizationService<C, E, D, R>>,
}
impl<C, E, D, R> LocalRemoteStartIdentity<C, E, D, R> {
    /// Builds at most 128 unique UTF-8 OCPP 1.6 idTags (1–20 characters each).
    /// # Errors
    /// Rejects oversized, duplicate, empty or invalid token material without exposing it.
    pub fn new(
        tokens: Vec<SensitiveAuthorizationToken>,
        policy: Arc<LocalAuthorizationService<C, E, D, R>>,
    ) -> Result<Self, StationCommandError> {
        let invalid = || StationCommandError::new("invalid remote start token configuration");
        if tokens.len() > 128 {
            return Err(invalid());
        }
        let mut entries = BTreeMap::new();
        for token in tokens {
            let text = std::str::from_utf8(token.expose_to_provider()).map_err(|_| invalid())?;
            if text.is_empty() || text.chars().count() > 20 {
                return Err(invalid());
            }
            let digest = Sha256::digest(token.expose_to_provider());
            let mut reference = String::from("sha256:");
            for byte in digest {
                use std::fmt::Write as _;
                write!(&mut reference, "{byte:02x}").expect("string write");
            }
            if entries.insert(reference, token).is_some() {
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
    ) -> Option<SensitiveAuthorizationToken> {
        let token = self.tokens.get(reference)?;
        let reference = AuthorizationReference::new(reference.to_owned()).ok()?;
        if !matches!(
            self.policy.authorize_reference(&reference, resource, now),
            AuthorizationDecision::Allowed { .. }
        ) {
            return None;
        }
        SensitiveAuthorizationToken::new(token.expose_to_provider()).ok()
    }
}
