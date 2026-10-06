use std::{
    future::pending,
    io::{self, Cursor},
    pin::Pin,
    task::{Context, Poll, ready},
};

use axum::{
    Router,
    body::{Body, BodyDataStream, Bytes},
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use futures_util::Stream;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, DuplexStream, ReadBuf};
use uob_application::artifact_provider::{
    ArtifactReference, ArtifactSha256, UploadId, UploadRefusal,
};

use super::{
    TestArtifactService,
    uploads::{BeginError, Completion},
};
use crate::artifacts::TransferError;

const STREAM_CHUNK_BYTES: usize = 16 * 1024;

/// Download and upload routes. Uploads accept PUT and POST with a raw body, with or without
/// the file name a station appends to the destination (OCPP 2.0.1 N01.FR.18 and N01.FR.21).
pub(super) fn router(service: TestArtifactService) -> Router {
    Router::new()
        .route("/artifacts/{reference}", get(download))
        .route("/uploads/{upload}", put(upload).post(upload))
        .route(
            "/uploads/{upload}/{file}",
            put(upload_named).post(upload_named),
        )
        .with_state(service)
}

async fn download(
    State(service): State<TestArtifactService>,
    Path(reference): Path<String>,
) -> Response {
    let Some(faults) = service.admit_transfer().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(bytes) = ArtifactReference::new(reference)
        .ok()
        .and_then(|reference| service.served_bytes(&reference, faults.corrupt_downloads))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let length = HeaderValue::from(bytes.len());
    let (writer, reader) = tokio::io::duplex(STREAM_CHUNK_BYTES);
    let transfers = service.state.transfers.clone();
    tokio::spawn(async move {
        // A refused or timed-out copy drops the writer, so the station sees a truncated body.
        let _ = transfers.copy(Cursor::new(bytes), writer, pending()).await;
    });
    let body = Body::from_stream(futures_util::stream::unfold(reader, next_chunk));
    let headers = [
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        ),
        (header::CONTENT_LENGTH, length),
    ];
    (headers, body).into_response()
}

async fn next_chunk(mut reader: DuplexStream) -> Option<(io::Result<Bytes>, DuplexStream)> {
    let mut chunk = vec![0; STREAM_CHUNK_BYTES];
    match reader.read(&mut chunk).await {
        Ok(0) => None,
        Ok(count) => {
            chunk.truncate(count);
            Some((Ok(Bytes::from(chunk)), reader))
        }
        Err(error) => Some((Err(error), reader)),
    }
}

async fn upload(
    State(service): State<TestArtifactService>,
    Path(upload): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> StatusCode {
    receive(&service, upload, &headers, body).await
}

async fn upload_named(
    State(service): State<TestArtifactService>,
    Path((upload, _file)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> StatusCode {
    receive(&service, upload, &headers, body).await
}

async fn receive(
    service: &TestArtifactService,
    upload: String,
    headers: &HeaderMap,
    body: Body,
) -> StatusCode {
    let Ok(upload_id) = UploadId::new(upload) else {
        return StatusCode::NOT_FOUND;
    };
    let Some(faults) = service.admit_transfer().await else {
        service.refuse_idle(&upload_id, UploadRefusal::Unavailable);
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let maximum = match service.begin_upload(&upload_id) {
        Ok(maximum) => maximum.min(faults.upload_cap.unwrap_or(u64::MAX)),
        Err(BeginError::Unknown) => return StatusCode::NOT_FOUND,
        Err(BeginError::Conflict) => return StatusCode::CONFLICT,
    };
    let mut attempt = Attempt {
        service,
        upload_id,
        finished: false,
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|declared| declared > maximum) {
        return attempt.refuse(TransferError::TooLarge);
    }
    // Narrowing fails only for a zero cap, which no upload fits.
    let Ok(transfers) = service.state.transfers.with_maximum_artifact_bytes(maximum) else {
        return attempt.refuse(TransferError::TooLarge);
    };
    let mut source = BodyReader {
        stream: body.into_data_stream(),
        chunk: Bytes::new(),
        hash: Sha256::new(),
    };
    match transfers
        .receive(&mut source, &service.state.spool, pending())
        .await
    {
        Ok(artifact) => {
            let sha256 = ArtifactSha256::from_bytes(source.hash.finalize().into());
            attempt.finish(Completion::Received(artifact, sha256));
            StatusCode::CREATED
        }
        Err(error) => attempt.refuse(error),
    }
}

/// Settles an upload attempt exactly once, including when the request future is dropped.
struct Attempt<'a> {
    service: &'a TestArtifactService,
    upload_id: UploadId,
    finished: bool,
}

impl Attempt<'_> {
    fn finish(&mut self, completion: Completion) {
        self.finished = true;
        self.service.finish_upload(&self.upload_id, completion);
    }

    fn refuse(&mut self, error: TransferError) -> StatusCode {
        let (refusal, status) = match error {
            TransferError::TooLarge => (UploadRefusal::TooLarge, StatusCode::PAYLOAD_TOO_LARGE),
            TransferError::TimedOut => (UploadRefusal::TimedOut, StatusCode::REQUEST_TIMEOUT),
            TransferError::Capacity | TransferError::InvalidLimits => {
                (UploadRefusal::Unavailable, StatusCode::SERVICE_UNAVAILABLE)
            }
            TransferError::Cancelled | TransferError::Io => {
                (UploadRefusal::Interrupted, StatusCode::BAD_REQUEST)
            }
        };
        self.finish(Completion::Refused(refusal));
        status
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(Completion::Refused(UploadRefusal::Interrupted));
        }
    }
}

/// Adapts a request body to `AsyncRead`, hashing exactly the bytes handed to the transfer.
struct BodyReader {
    stream: BodyDataStream,
    chunk: Bytes,
    hash: Sha256,
}

impl AsyncRead for BodyReader {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let reader = self.get_mut();
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        while reader.chunk.is_empty() {
            match ready!(Pin::new(&mut reader.stream).poll_next(context)) {
                Some(Ok(chunk)) => reader.chunk = chunk,
                Some(Err(_)) => return Poll::Ready(Err(io::Error::other("upload body failed"))),
                None => return Poll::Ready(Ok(())),
            }
        }
        let bytes = reader
            .chunk
            .split_to(reader.chunk.len().min(output.remaining()));
        reader.hash.update(&bytes);
        output.put_slice(&bytes);
        Poll::Ready(Ok(()))
    }
}
