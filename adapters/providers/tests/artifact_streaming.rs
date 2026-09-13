use std::{
    future::{pending, ready},
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use uob_application::{RuntimeResourceBudget, RuntimeResourceLimits, WorkClass};
use uob_provider_adapter::artifacts::{ArtifactTransfers, TransferError, TransferLimits};

fn budget() -> RuntimeResourceBudget {
    RuntimeResourceBudget::new(RuntimeResourceLimits {
        aggregate_queued_payload_bytes: 128 * 1024,
        reserved_critical_payload_bytes: 64 * 1024,
        trace_ring_bytes: 64 * 1024,
        ..RuntimeResourceLimits::default()
    })
    .unwrap()
}
fn transfers(budget: &RuntimeResourceBudget, maximum: u64, timeout: Duration) -> ArtifactTransfers {
    ArtifactTransfers::new(
        budget.clone(),
        TransferLimits {
            buffer_bytes: 64 * 1024,
            maximum_artifact_bytes: maximum,
            timeout,
        },
    )
    .unwrap()
}
struct PatternReader {
    remaining: u64,
    offset: u64,
    reads: Arc<AtomicUsize>,
}
impl AsyncRead for PatternReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let count = usize::try_from(self.remaining.min(output.remaining() as u64)).unwrap();
        for byte in output.initialize_unfilled_to(count) {
            *byte = u8::try_from(self.offset % 251).unwrap();
            self.offset += 1;
        }
        self.remaining -= count as u64;
        output.advance(count);
        Poll::Ready(Ok(()))
    }
}
struct PatternWriter {
    total: u64,
    budget: RuntimeResourceBudget,
}
impl AsyncWrite for PatternWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        assert_eq!(self.budget.snapshot().queued_payload_bytes, 64 * 1024);
        assert!(bytes.len() <= 64 * 1024);
        for byte in bytes {
            assert_eq!(*byte, u8::try_from(self.total % 251).unwrap());
            self.total += 1;
        }
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}
fn source(bytes: u64) -> PatternReader {
    PatternReader {
        remaining: bytes,
        offset: 0,
        reads: Arc::default(),
    }
}

#[tokio::test]
async fn artifact_larger_than_daemon_rss_budget_uses_one_small_buffer() {
    let budget = budget();
    let bytes = 129 * 1024 * 1024;
    let transfers = transfers(&budget, bytes, Duration::from_secs(60));
    let mut writer = PatternWriter {
        total: 0,
        budget: budget.clone(),
    };
    assert_eq!(
        transfers.copy(source(bytes), &mut writer, pending()).await,
        Ok(bytes)
    );
    assert_eq!(writer.total, bytes);
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}
#[tokio::test]
async fn size_limit_is_checked_before_writing_the_excess_chunk() {
    let budget = budget();
    let transfers = transfers(&budget, 4, Duration::from_secs(1));
    let mut output = Vec::new();
    assert_eq!(
        transfers.copy(&b"abcd"[..], &mut output, pending()).await,
        Ok(4)
    );
    assert_eq!(output, b"abcd");
    output.clear();
    assert_eq!(
        transfers.copy(&b"abcde"[..], &mut output, pending()).await,
        Err(TransferError::TooLarge)
    );
    assert!(output.is_empty());
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}
#[tokio::test]
async fn backpressure_stops_read_ahead_and_leaves_critical_capacity() {
    let budget = budget();
    let transfers = transfers(&budget, 1024 * 1024, Duration::from_secs(10));
    let reader = source(1024 * 1024);
    let reads = reader.reads.clone();
    let (destination, _stalled_peer) = tokio::io::duplex(1);
    let mut copy = Box::pin(transfers.copy(reader, destination, pending()));
    tokio::select! {
        result = &mut copy => panic!("unexpected completion: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(10)) => {},
    }
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    let charger = budget
        .try_reserve(WorkClass::ChargerRequest, 64 * 1024)
        .unwrap();
    assert_eq!(budget.snapshot().queued_payload_bytes, 128 * 1024);
    assert_eq!(
        transfers
            .copy(tokio::io::empty(), tokio::io::sink(), pending())
            .await,
        Err(TransferError::Capacity)
    );
    drop(charger);
    drop(copy);
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}
#[tokio::test]
async fn stalled_real_socket_can_be_cancelled_without_blocking_other_socket_work() {
    let budget = budget();
    let transfers = transfers(&budget, 1024 * 1024, Duration::from_secs(2));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (_stalled_peer, _) = listener.accept().await.unwrap();
    let (stop, cancelled) = tokio::sync::oneshot::channel();
    let copy = transfers.copy(client, tokio::io::sink(), async {
        let _ = cancelled.await;
    });
    let other_work = async {
        let _charger = budget.try_reserve(WorkClass::ChargerRequest, 1024).unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        client.write_all(b"heartbeat").await.unwrap();
        let mut message = [0; 9];
        peer.read_exact(&mut message).await.unwrap();
        assert_eq!(&message, b"heartbeat");
        stop.send(()).unwrap();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(copy, other_work)
    })
    .await
    .unwrap();
    assert_eq!(result, Err(TransferError::Cancelled));
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}
#[tokio::test(start_paused = true)]
async fn slow_trickle_does_not_extend_deadline_and_drop_releases_admission() {
    let budget = budget();
    let transfers = transfers(&budget, 1024, Duration::from_secs(10));
    let (mut peer, source) = tokio::io::duplex(16);
    let trickle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(4)).await;
            if peer.write_all(b"x").await.is_err() {
                break;
            }
        }
    });
    let start = tokio::time::Instant::now();
    assert_eq!(
        transfers.copy(source, tokio::io::sink(), pending()).await,
        Err(TransferError::TimedOut)
    );
    assert_eq!(start.elapsed(), Duration::from_secs(10));
    trickle.abort();
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    let (source, _peer) = tokio::io::duplex(1);
    let mut copy = Box::pin(transfers.copy(source, tokio::io::sink(), pending()));
    tokio::select! {
        result = &mut copy => panic!("unexpected completion: {result:?}"),
        () = tokio::time::sleep(Duration::from_secs(1)) => {},
    }
    assert_eq!(budget.snapshot().queued_payload_bytes, 64 * 1024);
    drop(copy);
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    assert_eq!(
        transfers
            .copy(tokio::io::empty(), tokio::io::sink(), ready(()))
            .await,
        Err(TransferError::Cancelled)
    );
}
struct FailedWriter(bool);
impl AsyncWrite for FailedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(if self.0 {
            Ok(bytes.len())
        } else {
            Err(io::Error::other("private credential"))
        })
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("private path")))
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}
#[tokio::test]
async fn write_and_flush_failures_are_sanitized_and_release_capacity() {
    let budget = budget();
    let transfers = transfers(&budget, 1024, Duration::from_secs(1));
    for writer in [FailedWriter(false), FailedWriter(true)] {
        assert_eq!(
            transfers.copy(&b"payload"[..], writer, pending()).await,
            Err(TransferError::Io)
        );
        assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    }
}
#[cfg(unix)]
#[path = "artifact_streaming/spool.rs"]
mod spool;
