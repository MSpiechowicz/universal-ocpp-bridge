//! Independent station-side checks: Security Whitepaper Ed4 L01.FR.04/12/23 and OCPP 2.0.1
//! errata L01.FR.04 (RSA-PSS or ECDSA over the entire file).
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rustls_pki_types::{CertificateDer, UnixTime, pem::PemObject};
use time::OffsetDateTime;
use webpki::{EndEntityCert, KeyUsage};

/// id-kp-codeSigning (RFC 5280 §4.2.1.12).
const CODE_SIGNING: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03];
const ROOT_LIMIT: usize = 4;
pub(crate) const ROOT_FILE_LIMIT: u64 = 32 * 1024;

/// Manufacturer roots installed on the simulated station; public material only.
#[derive(Clone)]
pub(crate) struct TrustRoots(Vec<CertificateDer<'static>>);

impl TrustRoots {
    /// # Errors
    /// Rejects an empty, oversized, unparseable or non-anchor root set.
    pub(crate) fn parse(pem: &[u8]) -> Result<Self, &'static str> {
        if pem.windows(11).any(|window| window == b"PRIVATE KEY") {
            return Err("firmware_root_invalid");
        }
        let roots = CertificateDer::pem_slice_iter(pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "firmware_root_invalid")?;
        if roots.is_empty()
            || roots.len() > ROOT_LIMIT
            || roots
                .iter()
                .any(|root| webpki::anchor_from_trusted_cert(root).is_err())
        {
            return Err("firmware_root_invalid");
        }
        Ok(Self(roots))
    }

    /// One code-signing leaf issued directly by an installed root and valid at `now`.
    pub(crate) fn certificate_trusted(&self, certificate: &str, now: OffsetDateTime) -> bool {
        self.verified(certificate, now, |_| true)
    }

    /// RSA-PSS with SHA-256 over the complete image using the trusted certificate's key.
    pub(crate) fn signature_valid(
        &self,
        certificate: &str,
        signature: &str,
        image: &[u8],
        now: OffsetDateTime,
    ) -> bool {
        let Ok(raw) = STANDARD.decode(signature) else {
            return false;
        };
        self.verified(certificate, now, |leaf| {
            leaf.verify_signature(
                webpki::aws_lc_rs::RSA_PSS_2048_8192_SHA256_LEGACY_KEY,
                image,
                &raw,
            )
            .is_ok()
        })
    }

    /// OCPP 2.0.1 errata L01.FR.04: RSA-PSS or ECDSA, with the hash of the signature algorithm,
    /// over the entire received file.
    pub(crate) fn signature_valid_201(
        &self,
        certificate: &str,
        signature: &str,
        image: &[u8],
        now: OffsetDateTime,
    ) -> bool {
        let Ok(raw) = STANDARD.decode(signature) else {
            return false;
        };
        self.verified(certificate, now, |leaf| {
            [
                webpki::aws_lc_rs::RSA_PSS_2048_8192_SHA256_LEGACY_KEY,
                webpki::aws_lc_rs::ECDSA_P256_SHA256,
                webpki::aws_lc_rs::ECDSA_P384_SHA384,
            ]
            .into_iter()
            .any(|algorithm| leaf.verify_signature(algorithm, image, &raw).is_ok())
        })
    }

    fn verified(
        &self,
        certificate: &str,
        now: OffsetDateTime,
        check: impl FnOnce(&EndEntityCert<'_>) -> bool,
    ) -> bool {
        let Ok(chain) =
            CertificateDer::pem_slice_iter(certificate.as_bytes()).collect::<Result<Vec<_>, _>>()
        else {
            return false;
        };
        // The manufacturer SHALL NOT use intermediates for the firmware signing certificate.
        let [leaf] = chain.as_slice() else {
            return false;
        };
        let Ok(leaf) = EndEntityCert::try_from(leaf) else {
            return false;
        };
        let Ok(anchors) = self
            .0
            .iter()
            .map(webpki::anchor_from_trusted_cert)
            .collect::<Result<Vec<_>, _>>()
        else {
            return false;
        };
        let Ok(seconds) = u64::try_from(now.unix_timestamp()) else {
            return false;
        };
        leaf.verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &anchors,
            &[],
            UnixTime::since_unix_epoch(std::time::Duration::from_secs(seconds)),
            KeyUsage::required(CODE_SIGNING),
            None,
            None,
        )
        .is_ok()
            && check(&leaf)
    }
}
