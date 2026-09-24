use super::{
    ProtocolEdition, RuntimeStation, StationAuthenticationMode, StationAuthenticator,
    StationCredential, StationRegistration, StationSecurityConfigurationError,
    StationSecurityConfigurationErrorCode,
};
use std::collections::BTreeMap;

impl StationAuthenticator {
    /// Builds an explicit credential-only demo allowlist without TLS references.
    /// # Errors
    /// Rejects empty, duplicate, or ambiguous identities and credentials.
    pub fn demo_with_protocols(
        registrations: Vec<(StationRegistration, ProtocolEdition, StationCredential)>,
    ) -> Result<Self, StationSecurityConfigurationError> {
        let invalid = || {
            StationSecurityConfigurationError::new(
                StationSecurityConfigurationErrorCode::ResolvedCredential,
                "stations.credential",
            )
        };
        if registrations.is_empty() {
            return Err(invalid());
        }
        let mut stations = BTreeMap::new();
        let mut references = std::collections::BTreeSet::new();
        let mut digests = std::collections::BTreeSet::new();
        for (registration, protocol, credential) in registrations {
            if registration.client_certificate.is_some()
                || !references.insert(registration.credential.as_str().to_owned())
                || !digests.insert(credential.0)
            {
                return Err(invalid());
            }
            if stations
                .insert(
                    registration.station_id.clone(),
                    RuntimeStation {
                        registration,
                        credential,
                        expected_protocol: Some(protocol),
                    },
                )
                .is_some()
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            authentication: StationAuthenticationMode::Credential,
            stations,
        })
    }
}
