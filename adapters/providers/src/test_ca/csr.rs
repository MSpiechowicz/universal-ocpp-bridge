use rcgen::{
    CertificateSigningRequestParams, DistinguishedName, DnType, DnValue, ExtendedKeyUsagePurpose,
    KeyUsagePurpose, PublicKeyData,
};
use rustls_pki_types::{CertificateSigningRequestDer, pem::PemObject};
use uob_application::certificate_provider::{
    CertificateChainPem, CertificateProviderError, CertificateSigningRequest, CertificateUse,
    CsrDecision, CsrRejection, SignedCertificate,
};
use x509_parser::{
    certification_request::X509CertificationRequest, prelude::FromDer, public_key::PublicKey,
};

use super::State;
use super::hierarchy::leaf_params;

/// Verifies an untrusted CSR and signs it with CA-chosen parameters.
///
/// The CSR must carry a valid self-signature, an accepted key, the expected common name and the
/// configured organization (OCPP 2.0.1 A00.FR.509 and A00.FR.511). Requested extensions are
/// never copied into the certificate.
pub(super) fn sign(
    state: &State,
    request: &CertificateSigningRequest,
) -> Result<CsrDecision, CertificateProviderError> {
    let csr = match parse(request) {
        Ok(csr) => csr,
        Err(rejection) => return Ok(CsrDecision::Rejected(rejection)),
    };
    let subject = &csr.params.distinguished_name;
    if text(subject, &DnType::CommonName) != Some(request.expected_common_name.as_str())
        || text(subject, &DnType::OrganizationName) != Some(state.organization.as_str())
    {
        return Ok(CsrDecision::Rejected(CsrRejection::SubjectMismatch));
    }
    let (issuer, extended_key_usages) = match request.certificate_use {
        CertificateUse::ChargingStation => (
            &state.hierarchy.cso_issuing,
            vec![ExtendedKeyUsagePurpose::ClientAuth],
        ),
        CertificateUse::V2g => (&state.hierarchy.v2g_root, Vec::new()),
    };
    let certificate = leaf_params(
        request.expected_common_name.as_str(),
        &state.organization,
        vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyAgreement,
        ],
        extended_key_usages,
    )
    .signed_by(&csr.public_key, &issuer.issuer)
    .map_err(|_| CertificateProviderError::Unavailable)?;
    let mut chain = certificate.pem();
    if request.certificate_use == CertificateUse::ChargingStation {
        chain.push_str(issuer.pem.as_str());
    }
    Ok(CsrDecision::Signed(SignedCertificate {
        chain: CertificateChainPem::new(chain)
            .map_err(|_| CertificateProviderError::InvalidProviderResponse)?,
        test_only: true,
    }))
}

fn parse(
    request: &CertificateSigningRequest,
) -> Result<CertificateSigningRequestParams, CsrRejection> {
    let der = CertificateSigningRequestDer::from_pem_slice(request.csr.as_str().as_bytes())
        .map_err(|_| CsrRejection::Malformed)?;
    let (rest, decoded) =
        X509CertificationRequest::from_der(der.as_ref()).map_err(|_| CsrRejection::Malformed)?;
    if !rest.is_empty() {
        return Err(CsrRejection::Malformed);
    }
    match decoded.certification_request_info.subject_pki.parsed() {
        Ok(PublicKey::RSA(key)) if key.key_size() >= 2048 => {}
        Ok(PublicKey::EC(point)) if matches!(point.key_size(), 256 | 384) => {}
        Ok(_) => return Err(CsrRejection::UnsupportedKey),
        Err(_) => return Err(CsrRejection::Malformed),
    }
    let csr = CertificateSigningRequestParams::from_der(&der).map_err(|error| match error {
        rcgen::Error::RingUnspecified => CsrRejection::InvalidSignature,
        rcgen::Error::UnsupportedExtension => CsrRejection::UnsupportedRequest,
        rcgen::Error::UnsupportedSignatureAlgorithm => CsrRejection::UnsupportedKey,
        _ => CsrRejection::Malformed,
    })?;
    // rcgen derives the key algorithm from the CSR signature algorithm, so a P-384 key signed
    // with SHA-256 would be re-encoded with the P-256 curve. Sign only keys it reproduces exactly.
    if csr.public_key.subject_public_key_info()
        != decoded.certification_request_info.subject_pki.raw
    {
        return Err(CsrRejection::UnsupportedKey);
    }
    Ok(csr)
}

fn text<'a>(name: &'a DistinguishedName, kind: &DnType) -> Option<&'a str> {
    match name.get(kind)? {
        DnValue::Utf8String(value) => Some(value),
        DnValue::PrintableString(value) => Some(value.as_str()),
        DnValue::Ia5String(value) => Some(value.as_str()),
        _ => None,
    }
}
