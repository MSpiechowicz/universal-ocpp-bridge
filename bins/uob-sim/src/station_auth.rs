use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use ocpp_client::{
    ConnectOptions, TransportError, TransportSink, TransportStream, websocket_transport,
};

const MAX_CREDENTIAL_BYTES: u64 = 4096;
const MIN_CREDENTIAL_BYTES: usize = 16;
const CREDENTIAL_UNAVAILABLE: &str = "station credential unavailable";
const HANDSHAKE_DENIED: &str = "station handshake failed";

struct Secret(Vec<u8>);

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

fn unavailable() -> TransportError {
    Box::new(io::Error::other(CREDENTIAL_UNAVAILABLE))
}

fn read_secret(path: &str) -> Result<Secret, TransportError> {
    let before = fs::symlink_metadata(path).map_err(|_| unavailable())?;
    if !before.is_file()
        || !(MIN_CREDENTIAL_BYTES as u64..=MAX_CREDENTIAL_BYTES).contains(&before.len())
    {
        return Err(unavailable());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| unavailable())?;
    let metadata = file.metadata().map_err(|_| unavailable())?;
    if !metadata.is_file()
        || metadata.dev() != before.dev()
        || metadata.ino() != before.ino()
        || metadata.len() != before.len()
    {
        return Err(unavailable());
    }

    let mut secret = Secret(Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| unavailable())?,
    ));
    file.take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut secret.0)
        .map_err(|_| unavailable())?;
    if secret.0.len() < MIN_CREDENTIAL_BYTES
        || secret.0.len() as u64 != metadata.len()
        || secret.0.len() as u64 > MAX_CREDENTIAL_BYTES
        || std::str::from_utf8(&secret.0).is_err()
    {
        return Err(unavailable());
    }
    Ok(secret)
}

pub(super) async fn connect(
    endpoint: &str,
    version: ocpp_client::OcppVersion,
    credentials_file: Option<&str>,
) -> Result<(Box<dyn TransportSink>, Box<dyn TransportStream>), TransportError> {
    let Some(path) = credentials_file else {
        return websocket_transport(endpoint, version, None).await;
    };

    let url = url::Url::parse(endpoint).map_err(|_| unavailable())?;
    let station = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|id| !id.is_empty() && !id.contains('%') && !id.contains(':'))
        .ok_or_else(unavailable)?;
    let secret = read_secret(path)?;
    let password = std::str::from_utf8(&secret.0).map_err(|_| unavailable())?;
    let options = ConnectOptions {
        username: Some(station),
        password: Some(password),
        ..ConnectOptions::default()
    };
    websocket_transport(endpoint, version, Some(options))
        .await
        .map_err(|_| Box::new(io::Error::other(HANDSHAKE_DENIED)) as TransportError)
}
