use std::{error::Error, fmt, sync::Arc};

use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

use super::{
    StationAuthenticationMode, StationSecurityConfigurationError,
    StationSecurityConfigurationErrorCode,
};

/// In-memory certificate and key values loaded from validated credential references.
pub struct StationTlsMaterial {
    /// Server end-entity certificate followed by its issuer chain.
    pub server_certificate_chain: Vec<CertificateDer<'static>>,
    /// Private key matching the server end-entity certificate.
    pub server_private_key: PrivateKeyDer<'static>,
    /// Client trust anchors required only for mutual TLS.
    pub client_certificate_authorities: Vec<CertificateDer<'static>>,
}

/// Cloneable TLS acceptor configured to require a valid client chain when mTLS is enabled.
#[derive(Clone)]
pub struct StationTlsAcceptor(TlsAcceptor);

/// Deliberately opaque handshake rejection safe for untrusted responses and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StationTlsHandshakeError;

impl fmt::Display for StationTlsHandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("station TLS handshake rejected")
    }
}

impl Error for StationTlsHandshakeError {}

impl StationTlsAcceptor {
    /// Performs a TLS handshake before any HTTP, WebSocket, or station task allocation.
    ///
    /// # Errors
    ///
    /// Missing, untrusted, not-yet-valid, and expired client certificates are rejected by rustls.
    pub async fn accept<IO>(&self, stream: IO) -> Result<TlsStream<IO>, StationTlsHandshakeError>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        self.0
            .accept(stream)
            .await
            .map_err(|_| StationTlsHandshakeError)
    }
}

/// Builds the TLS boundary from already loaded material without exposing parser or file I/O types.
///
/// # Errors
///
/// Rejects missing/invalid server material or inconsistent client trust anchors with safe context.
pub fn build_station_tls_acceptor(
    authentication: StationAuthenticationMode,
    material: StationTlsMaterial,
) -> Result<StationTlsAcceptor, StationSecurityConfigurationError> {
    if material.server_certificate_chain.is_empty() {
        return Err(configuration_error(
            StationSecurityConfigurationErrorCode::ServerTlsMaterial,
            "tls.server_certificate_chain",
        ));
    }
    let builder = ServerConfig::builder();
    let builder = match authentication {
        StationAuthenticationMode::Credential => {
            if !material.client_certificate_authorities.is_empty() {
                return Err(configuration_error(
                    StationSecurityConfigurationErrorCode::ClientCertificateAuthorities,
                    "tls.client_certificate_authorities",
                ));
            }
            builder.with_no_client_auth()
        }
        StationAuthenticationMode::CredentialAndMutualTls => {
            let mut roots = RootCertStore::empty();
            if material.client_certificate_authorities.is_empty()
                || material
                    .client_certificate_authorities
                    .into_iter()
                    .any(|certificate| roots.add(certificate).is_err())
            {
                return Err(configuration_error(
                    StationSecurityConfigurationErrorCode::ClientCertificateAuthorities,
                    "tls.client_certificate_authorities",
                ));
            }
            let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|_| {
                    configuration_error(
                        StationSecurityConfigurationErrorCode::ClientCertificateAuthorities,
                        "tls.client_certificate_authorities",
                    )
                })?;
            builder.with_client_cert_verifier(verifier)
        }
    };
    let configuration = builder
        .with_single_cert(
            material.server_certificate_chain,
            material.server_private_key,
        )
        .map_err(|_| {
            configuration_error(
                StationSecurityConfigurationErrorCode::ServerTlsMaterial,
                "tls.server_private_key",
            )
        })?;
    Ok(StationTlsAcceptor(TlsAcceptor::from(Arc::new(
        configuration,
    ))))
}

const fn configuration_error(
    code: StationSecurityConfigurationErrorCode,
    field: &'static str,
) -> StationSecurityConfigurationError {
    StationSecurityConfigurationError::new(code, field)
}
