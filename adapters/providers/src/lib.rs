#![doc = "External provider boundary for authorization, PKI, artifacts, and payments."]

use sha2::{Digest, Sha256};
use uob_application::{
    AuthorizationProvider, AuthorizationProviderDescriptor, AuthorizationProviderFuture,
    AuthorizationReference, SensitiveAuthorizationToken,
};

/// Marker for providers selected by the composition root.
pub trait ProviderAdapter: Send + Sync {
    /// Stable provider kind.
    fn kind(&self) -> &'static str;
}

/// Offline provider that derives a non-reversible local allowlist reference from token bytes.
#[derive(Clone, Copy, Default)]
pub struct LocalAuthorizationProvider;

impl AuthorizationProvider for LocalAuthorizationProvider {
    fn descriptor(&self) -> AuthorizationProviderDescriptor {
        AuthorizationProviderDescriptor {
            kind: "local.sha256",
            test_only: false,
        }
    }

    fn resolve<'a>(
        &'a self,
        token: &'a SensitiveAuthorizationToken,
    ) -> AuthorizationProviderFuture<'a> {
        Box::pin(async move {
            let digest = Sha256::digest(token.expose_to_provider());
            let mut reference = String::with_capacity(7 + digest.len() * 2);
            reference.push_str("sha256:");
            for byte in digest {
                use std::fmt::Write as _;
                write!(&mut reference, "{byte:02x}").expect("writing to a String cannot fail");
            }
            AuthorizationReference::new(reference)
                .map_err(|_| uob_application::AuthorizationProviderError::InvalidToken)
        })
    }
}

impl ProviderAdapter for LocalAuthorizationProvider {
    fn kind(&self) -> &'static str {
        "local.sha256"
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };

    use uob_application::{AuthorizationProvider, SensitiveAuthorizationToken};

    use super::LocalAuthorizationProvider;

    #[test]
    fn local_provider_derives_stable_reference_without_retaining_token() {
        let token = SensitiveAuthorizationToken::new("station-card-secret").expect("token");
        let reference = block_on(LocalAuthorizationProvider.resolve(&token)).expect("reference");
        assert_eq!(
            reference.as_str(),
            "sha256:6b23af14c0afa06576c0acc18b967b4e69967fceaafa7099578799a866d02ef9"
        );
        assert!(!LocalAuthorizationProvider.descriptor().test_only);
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = pin!(future);
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
            std::thread::yield_now();
        }
    }
}
