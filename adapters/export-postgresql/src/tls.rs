use std::sync::Arc;

use postgres_rustls::MakeTlsConnector;
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, pem::PemObject as _},
};
use uob_application::DatabaseError;

use crate::errors::invalid;

pub(crate) fn connector(pem: &[u8]) -> Result<MakeTlsConnector, DatabaseError> {
    let mut roots = RootCertStore::empty();
    let mut count = 0;
    for cert in CertificateDer::pem_slice_iter(pem) {
        let cert = cert.map_err(|_| invalid("postgres.ca.invalid"))?;
        roots
            .add(cert)
            .map_err(|_| invalid("postgres.ca.invalid"))?;
        count += 1;
        if count > 16 {
            return Err(invalid("postgres.ca.invalid"));
        }
    }
    if count == 0 {
        return Err(invalid("postgres.ca.invalid"));
    }
    let config = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| invalid("postgres.tls.invalid"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(MakeTlsConnector::new(tokio_rustls::TlsConnector::from(
        Arc::new(config),
    )))
}
