use super::peer::{FRAME_LIMIT, Socket};
use serde::Deserialize;
use std::{
    fs::OpenOptions,
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub url: String,
    pub authorization_file: Option<PathBuf>,
    pub state_file: PathBuf,
    #[serde(default = "default_evse")]
    pub phase_evse: u32,
    #[serde(default)]
    pub phase_supported: bool,
}

fn default_evse() -> u32 {
    1
}

pub fn startup<T>(stage: &'static str, result: io::Result<T>) -> io::Result<T> {
    result.inspect_err(|error| diagnostic(stage, io_kind(error.kind())))
}

fn diagnostic(stage: &'static str, kind: &'static str) {
    println!("{{\"event\":\"startup_failed\",\"stage\":\"{stage}\",\"kind\":\"{kind}\"}}");
}

fn io_kind(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::ConnectionRefused => "connection_refused",
        io::ErrorKind::ConnectionReset => "connection_reset",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::UnexpectedEof => "unexpected_eof",
        _ => "other",
    }
}

fn private_read(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > 16 * 1024
    {
        return Err(io::Error::other("private peer configuration required"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(io::Error::other("private input bound"));
    }
    Ok(bytes)
}

pub fn configuration() -> io::Result<Config> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or_else(|| io::Error::other("configuration path required"))?;
    if args.next().is_some() {
        return Err(io::Error::other("unexpected argument"));
    }
    let config: Config = serde_json::from_slice(&private_read(Path::new(&path))?)
        .map_err(|_| io::Error::other("invalid private peer configuration"))?;
    let url =
        reqwest::Url::parse(&config.url).map_err(|_| io::Error::other("invalid loopback URL"))?;
    if url.scheme() != "ws"
        || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || config.phase_evse == 0
        || i32::try_from(config.phase_evse).is_err()
    {
        return Err(io::Error::other("private loopback peer URL required"));
    }
    Ok(config)
}

pub async fn connect(config: &Config) -> io::Result<Socket> {
    let mut request = startup(
        "connection_request",
        config
            .url
            .as_str()
            .into_client_request()
            .map_err(|_| io::Error::other("invalid connection request")),
    )?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "ocpp2.0.1".parse().unwrap());
    if let Some(path) = &config.authorization_file {
        let bytes = startup("authorization_file", private_read(path))?;
        let text = startup(
            "authorization_encoding",
            std::str::from_utf8(&bytes).map_err(|_| io::Error::other("invalid authorization")),
        )?;
        let header = startup(
            "authorization_header",
            text.trim_end()
                .parse()
                .map_err(|_| io::Error::other("invalid authorization")),
        )?;
        request.headers_mut().insert("Authorization", header);
    }
    let limits = WebSocketConfig::default()
        .max_message_size(Some(FRAME_LIMIT))
        .max_frame_size(Some(FRAME_LIMIT));
    let (socket, response) = connect_async_with_config(request, Some(limits), false)
        .await
        .map_err(|error| {
            let kind = match error {
                tokio_tungstenite::tungstenite::Error::Io(error) => io_kind(error.kind()),
                tokio_tungstenite::tungstenite::Error::Http(_) => "http_rejected",
                _ => "websocket_error",
            };
            diagnostic("websocket_handshake", kind);
            io::Error::other("independent peer connection failed")
        })?;
    if response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        != Some("ocpp2.0.1")
    {
        return startup(
            "websocket_subprotocol",
            Err(io::Error::other("independent peer subprotocol mismatch")),
        );
    }
    Ok(socket)
}

pub async fn line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> io::Result<Option<Vec<u8>>> {
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(std::mem::take(line)))
            };
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(bytes.len(), |i| i + 1);
        if line.len() + count > 64 * 1024 {
            return Err(io::Error::other("peer control bound"));
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(Some(std::mem::take(line)));
        }
    }
}
