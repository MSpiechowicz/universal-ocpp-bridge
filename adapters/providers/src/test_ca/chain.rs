use std::time::Duration;

use rustls_pki_types::{CertificateDer, UnixTime, pem::PemObject};
use uob_application::certificate_provider::{
    ChainPurpose, ChainVerification, TrustDecision, TrustFailure,
};
use webpki::{EndEntityCert, Error, KeyUsage};
use x509_parser::{certificate::X509Certificate, prelude::FromDer, x509::AttributeTypeAndValue};

use super::State;

/// DER content of id-kp-codeSigning (1.3.6.1.5.5.7.3.3).
const CODE_SIGNING: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03];

/// Decides trust against the root configured for the purpose, then checks the leaf subject.
pub(super) fn verify(state: &State, request: &ChainVerification) -> TrustDecision {
    match decide(state, request) {
        Ok(()) => TrustDecision::Trusted,
        Err(failure) => TrustDecision::Untrusted(failure),
    }
}

fn decide(state: &State, request: &ChainVerification) -> Result<(), TrustFailure> {
    let certificates = CertificateDer::pem_slice_iter(request.chain.as_str().as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TrustFailure::Malformed)?;
    let (leaf, intermediates) = certificates.split_first().ok_or(TrustFailure::Malformed)?;
    let hierarchy = &state.hierarchy;
    let root = match request.purpose {
        ChainPurpose::ChargingStation => &hierarchy.cso_root,
        ChainPurpose::FirmwareSigning => &hierarchy.manufacturer_root,
        ChainPurpose::V2g => &hierarchy.v2g_root,
    };
    let anchor =
        webpki::anchor_from_trusted_cert(root.issuer.der()).map_err(|_| TrustFailure::Malformed)?;
    let end_entity = EndEntityCert::try_from(leaf).map_err(|_| TrustFailure::Malformed)?;
    let seconds = u64::try_from(request.at.into_inner().unix_timestamp())
        .map_err(|_| TrustFailure::NotYetValid)?;
    let at = UnixTime::since_unix_epoch(Duration::from_secs(seconds));
    let anchors = [anchor];
    let verified = match request.purpose {
        ChainPurpose::ChargingStation => end_entity.verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &anchors,
            intermediates,
            at,
            KeyUsage::client_auth(),
            None,
            None,
        ),
        ChainPurpose::FirmwareSigning => end_entity.verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &anchors,
            intermediates,
            at,
            KeyUsage::required(CODE_SIGNING),
            None,
            None,
        ),
        ChainPurpose::V2g => end_entity.verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &anchors,
            intermediates,
            at,
            KeyUsage::server_auth(),
            None,
            None,
        ),
    };
    verified.map_err(|error| failure(&error))?;
    check_subject(state, request, leaf)
}

/// OCPP 2.0.1 A00.FR.404 and A00.FR.405: the CSO name and the expected station serial.
fn check_subject(
    state: &State,
    request: &ChainVerification,
    leaf: &CertificateDer<'_>,
) -> Result<(), TrustFailure> {
    let (_, certificate) = X509Certificate::from_der(leaf).map_err(|_| TrustFailure::Malformed)?;
    let subject = certificate.subject();
    let organization_matches = request.purpose != ChainPurpose::ChargingStation
        || single(subject.iter_organization()) == Some(state.organization.as_str());
    let common_name_matches = request
        .expected_common_name
        .as_ref()
        .is_none_or(|expected| single(subject.iter_common_name()) == Some(expected.as_str()));
    if organization_matches && common_name_matches {
        Ok(())
    } else {
        Err(TrustFailure::SubjectMismatch)
    }
}

/// The only value of a subject attribute; repeated attributes never match.
fn single<'a>(mut values: impl Iterator<Item = &'a AttributeTypeAndValue<'a>>) -> Option<&'a str> {
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.as_str().ok()
}

fn failure(error: &Error) -> TrustFailure {
    match error {
        Error::CertExpired { .. } => TrustFailure::Expired,
        Error::CertNotValidYet { .. } => TrustFailure::NotYetValid,
        Error::RequiredEkuNotFoundContext(_) | Error::CaUsedAsEndEntity => {
            TrustFailure::WrongPurpose
        }
        Error::InvalidSignatureForPublicKey
        | Error::SignatureAlgorithmMismatch
        | Error::UnsupportedSignatureAlgorithmContext(_)
        | Error::UnsupportedSignatureAlgorithmForPublicKeyContext(_) => {
            TrustFailure::InvalidSignature
        }
        Error::MaximumPathDepthExceeded
        | Error::MaximumPathBuildCallsExceeded
        | Error::MaximumSignatureChecksExceeded => TrustFailure::ChainTooLong,
        Error::BadDer | Error::BadDerTime | Error::TrailingData(_) => TrustFailure::Malformed,
        _ => TrustFailure::UnknownIssuer,
    }
}
