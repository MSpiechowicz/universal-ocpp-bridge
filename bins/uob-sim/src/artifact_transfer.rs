//! Station-side HTTP artifact transfers: firmware download and log upload.
//!
//! Transfers use only the location a CSMS sent. Each attempt caps the bytes it holds, follows no
//! redirects or proxies, and is bounded by one deadline. Failures never echo the location.

use std::time::Duration;

use reqwest::{Client, Response, header::CONTENT_TYPE, redirect::Policy};
use sha2::{Digest, Sha256};
use url::Url;

/// Bounds applied to one transfer attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferPolicy {
    pub maximum_bytes: u64,
    pub timeout: Duration,
}

/// Complete downloaded artifact and the digest of exactly those bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadedArtifact {
    pub bytes: Vec<u8>,
    pub sha256: [u8; 32],
}

/// Sanitized failure without the location, credentials or payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferFailure {
    InvalidLocation,
    Unreachable,
    Status(u16),
    TooLarge,
    TimedOut,
    Interrupted,
}

/// Downloads a complete artifact from an `http` or `https` location.
///
/// # Errors
///
/// Returns a sanitized failure for an invalid location, a non-success status, more than the
/// policy's bytes, an expired deadline or a broken connection.
pub async fn download(
    location: &str,
    policy: TransferPolicy,
) -> Result<DownloadedArtifact, TransferFailure> {
    let url = location_url(location)?;
    let client = client(policy)?;
    bounded(policy, async {
        let mut response = success(
            client
                .get(url)
                .send()
                .await
                .map_err(|error| failure(&error))?,
        )?;
        if response
            .content_length()
            .is_some_and(|length| length > policy.maximum_bytes)
        {
            return Err(TransferFailure::TooLarge);
        }
        let mut bytes = Vec::new();
        let mut hash = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| failure(&error))? {
            if u64::try_from(bytes.len() + chunk.len())
                .map_or(true, |size| size > policy.maximum_bytes)
            {
                return Err(TransferFailure::TooLarge);
            }
            hash.update(&chunk);
            bytes.extend_from_slice(&chunk);
        }
        Ok(DownloadedArtifact {
            bytes,
            sha256: hash.finalize().into(),
        })
    })
    .await
}

/// Uploads one log file with HTTP PUT. When the location ends with `/`, the file name is
/// appended, as OCPP 2.0.1 N01.FR.21 requires.
///
/// # Errors
///
/// Returns a sanitized failure for an invalid location or file name, more than the policy's
/// bytes, a non-success status, an expired deadline or a broken connection.
pub async fn upload(
    location: &str,
    file_name: &str,
    bytes: Vec<u8>,
    policy: TransferPolicy,
) -> Result<(), TransferFailure> {
    if u64::try_from(bytes.len()).map_or(true, |size| size > policy.maximum_bytes) {
        return Err(TransferFailure::TooLarge);
    }
    let mut target = location.to_owned();
    if target.ends_with('/') {
        if file_name.is_empty()
            || file_name.starts_with('.')
            || !file_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(TransferFailure::InvalidLocation);
        }
        target.push_str(file_name);
    }
    let url = location_url(&target)?;
    let client = client(policy)?;
    bounded(policy, async {
        let request = client
            .put(url)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(bytes);
        success(request.send().await.map_err(|error| failure(&error))?).map(drop)
    })
    .await
}

fn location_url(location: &str) -> Result<Url, TransferFailure> {
    let url = Url::parse(location).map_err(|_| TransferFailure::InvalidLocation)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(TransferFailure::InvalidLocation);
    }
    Ok(url)
}

fn client(policy: TransferPolicy) -> Result<Client, TransferFailure> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(policy.timeout)
        .build()
        .map_err(|_| TransferFailure::Unreachable)
}

async fn bounded<T>(
    policy: TransferPolicy,
    transfer: impl Future<Output = Result<T, TransferFailure>>,
) -> Result<T, TransferFailure> {
    tokio::time::timeout(policy.timeout, transfer)
        .await
        .map_err(|_| TransferFailure::TimedOut)?
}

fn success(response: Response) -> Result<Response, TransferFailure> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(TransferFailure::Status(response.status().as_u16()))
    }
}

fn failure(error: &reqwest::Error) -> TransferFailure {
    if error.is_timeout() {
        TransferFailure::TimedOut
    } else if error.is_connect() {
        TransferFailure::Unreachable
    } else {
        TransferFailure::Interrupted
    }
}
