use std::{error::Error, fmt, fs, io::Write};

use futures_util::StreamExt;
use reqwest::{Client, header};

use crate::configuration::ValidatedEventClientConfiguration;

const MAX_CREDENTIAL_BYTES: usize = 8 * 1024;
const MAX_PENDING_LINE_BYTES: usize = 64 * 1024;
const MAX_EVENT_BYTES: usize = 256 * 1024;

pub(crate) async fn stream(
    configuration: &ValidatedEventClientConfiguration,
    after: Option<&str>,
    output: &mut impl Write,
) -> Result<(), EventStreamError> {
    let credential = configuration
        .credentials_file
        .as_deref()
        .map(read_credential)
        .transpose()?;
    let mut endpoint = configuration.endpoint.clone();
    if let Some(cursor) = after {
        endpoint.query_pairs_mut().append_pair("after", cursor);
    }
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| EventStreamError::ClientSetup)?;
    let mut request = client
        .get(endpoint)
        .header(header::ACCEPT, "text/event-stream");
    if let Some(credential) = &credential {
        request = request.bearer_auth(credential);
    }
    let response = request
        .send()
        .await
        .map_err(|_| EventStreamError::Connection)?;
    if !response.status().is_success() {
        return Err(EventStreamError::Rejected);
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
    {
        return Err(EventStreamError::InvalidContentType);
    }

    let mut decoder = SseDecoder::default();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        decoder.push(
            &chunk.map_err(|_| EventStreamError::Connection)?,
            credential.as_deref(),
            output,
        )?;
    }
    decoder.finish(credential.as_deref(), output)
}

fn read_credential(path: &str) -> Result<String, EventStreamError> {
    let bytes = fs::read(path).map_err(|_| EventStreamError::CredentialUnavailable)?;
    if bytes.len() > MAX_CREDENTIAL_BYTES {
        return Err(EventStreamError::InvalidCredential);
    }
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| EventStreamError::InvalidCredential)?
        .trim();
    if value.is_empty() || value.contains(['\r', '\n']) {
        return Err(EventStreamError::InvalidCredential);
    }
    Ok(value.to_owned())
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<u8>,
}

impl SseDecoder {
    fn push(
        &mut self,
        chunk: &[u8],
        credential: Option<&str>,
        output: &mut impl Write,
    ) -> Result<(), EventStreamError> {
        for byte in chunk {
            if *byte == b'\n' {
                let mut line = std::mem::take(&mut self.pending);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.line(&line, credential, output)?;
            } else {
                if self.pending.len() == MAX_PENDING_LINE_BYTES {
                    return Err(EventStreamError::EventTooLarge);
                }
                self.pending.push(*byte);
            }
        }
        Ok(())
    }

    fn line(
        &mut self,
        line: &[u8],
        credential: Option<&str>,
        output: &mut impl Write,
    ) -> Result<(), EventStreamError> {
        if line.is_empty() {
            return self.dispatch(credential, output);
        }
        let Some(value) = line.strip_prefix(b"data:") else {
            return Ok(());
        };
        let value = value.strip_prefix(b" ").unwrap_or(value);
        if !self.data.is_empty() {
            self.data.push(b'\n');
        }
        if self.data.len().saturating_add(value.len()) > MAX_EVENT_BYTES {
            return Err(EventStreamError::EventTooLarge);
        }
        self.data.extend_from_slice(value);
        Ok(())
    }

    fn dispatch(
        &mut self,
        credential: Option<&str>,
        output: &mut impl Write,
    ) -> Result<(), EventStreamError> {
        if self.data.is_empty() {
            return Ok(());
        }
        let value: serde_json::Value =
            serde_json::from_slice(&self.data).map_err(|_| EventStreamError::InvalidEvent)?;
        let encoded = serde_json::to_vec(&value).map_err(|_| EventStreamError::InvalidEvent)?;
        if credential.is_some_and(|secret| contains(&encoded, secret.as_bytes())) {
            return Err(EventStreamError::CredentialReflected);
        }
        output
            .write_all(&encoded)
            .and_then(|()| output.write_all(b"\n"))
            .map_err(|_| EventStreamError::Output)?;
        self.data.clear();
        Ok(())
    }

    fn finish(
        mut self,
        credential: Option<&str>,
        output: &mut impl Write,
    ) -> Result<(), EventStreamError> {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.line(&pending, credential, output)?;
        }
        self.dispatch(credential, output)
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EventStreamError {
    CredentialUnavailable,
    InvalidCredential,
    ClientSetup,
    Connection,
    Rejected,
    InvalidContentType,
    EventTooLarge,
    InvalidEvent,
    CredentialReflected,
    Output,
}

impl fmt::Display for EventStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "event stream {self:?}")
    }
}

impl Error for EventStreamError {}

#[cfg(test)]
mod tests {
    use super::{EventStreamError, SseDecoder};

    #[test]
    fn chunked_multiline_sse_becomes_compact_jsonl() {
        let mut decoder = SseDecoder::default();
        let mut output = Vec::new();
        decoder
            .push(b"id: cursor-1\r\ndata: {\"kind\":\"", None, &mut output)
            .unwrap();
        decoder
            .push(b"event\",\r\ndata: \"value\":1}\r\n\r\n", None, &mut output)
            .unwrap();

        assert_eq!(output, b"{\"kind\":\"event\",\"value\":1}\n");
    }

    #[test]
    fn reflected_bearer_value_never_reaches_stdout() {
        let mut decoder = SseDecoder::default();
        let mut output = Vec::new();
        let error = decoder
            .push(
                b"data: {\"token\":\"private-token\"}\n\n",
                Some("private-token"),
                &mut output,
            )
            .unwrap_err();

        assert_eq!(error, EventStreamError::CredentialReflected);
        assert!(output.is_empty());
    }
}
