//! Bounded provider-side byte transport for firmware and diagnostic workflows.
//! Callers own authorization, transport/TLS policy, integrity checks and workflow decisions.

use std::{future::Future, time::Duration};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uob_application::{RuntimeReservation, RuntimeResourceBudget, WorkClass};

#[cfg(unix)]
mod spool;
#[cfg(unix)]
pub use spool::TemporaryArtifact;

/// Per-transfer policy; the byte cap applies to the whole artifact, not each chunk.
#[derive(Clone, Copy, Debug)]
pub struct TransferLimits {
    pub buffer_bytes: usize,
    pub maximum_artifact_bytes: u64,
    pub timeout: Duration,
}

/// Sanitized failures contain no payloads, credentials, URLs or filesystem paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferError {
    InvalidLimits,
    Capacity,
    TooLarge,
    Cancelled,
    TimedOut,
    Io,
}

/// A shared admission authority, with no internal transfer queue.
#[derive(Clone, Debug)]
pub struct ArtifactTransfers {
    budget: RuntimeResourceBudget,
    limits: TransferLimits,
}

// Buffer and its reservation travel together, including into blocking file jobs. Fields drop
// in declaration order: storage is released before its admission reservation.
struct Buffer {
    bytes: Vec<u8>,
    _reservation: RuntimeReservation,
}

impl ArtifactTransfers {
    /// Configure bounded transfers. Background transfers share the existing multipart slots
    /// and cannot consume the capacity reserved for charger requests and critical reports.
    ///
    /// # Errors
    /// Returns `InvalidLimits` for zero limits, chunks over 256 KiB, or unrepresentable deadlines.
    pub fn new(
        budget: RuntimeResourceBudget,
        limits: TransferLimits,
    ) -> Result<Self, TransferError> {
        if limits.buffer_bytes == 0
            || limits.buffer_bytes > 256 * 1024
            || limits.maximum_artifact_bytes == 0
            || limits.timeout.is_zero()
            || tokio::time::Instant::now()
                .checked_add(limits.timeout)
                .is_none()
        {
            return Err(TransferError::InvalidLimits);
        }
        Ok(Self { budget, limits })
    }

    fn reserve(&self) -> Result<Buffer, TransferError> {
        let reservation = self
            .budget
            .try_reserve(WorkClass::MultipartAssembly, self.limits.buffer_bytes)
            .map_err(|_| TransferError::Capacity)?;
        Ok(Buffer {
            bytes: vec![0; self.limits.buffer_bytes],
            _reservation: reservation,
        })
    }

    /// Stream between owned provider transports, applying backpressure after every chunk.
    /// Completes only after EOF and destination flush; it does not imply firmware installation.
    /// Dropping this future drops its transports and buffer. Transport-internal buffers must
    /// be bounded independently by their adapters. Partial writes are never retried here.
    ///
    /// # Errors
    /// Returns a sanitized capacity, size, I/O, cancellation or absolute-deadline failure.
    pub async fn copy<R, W, C>(
        &self,
        mut source: R,
        mut destination: W,
        cancelled: C,
    ) -> Result<u64, TransferError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
        C: Future<Output = ()>,
    {
        bounded(self.limits.timeout, cancelled, async {
            let mut buffer = self.reserve()?;
            let mut total = 0;
            loop {
                let count = source
                    .read(&mut buffer.bytes)
                    .await
                    .map_err(|_| TransferError::Io)?;
                total = checked_size(total, count, self.limits.maximum_artifact_bytes)?;
                if count == 0 {
                    destination.flush().await.map_err(|_| TransferError::Io)?;
                    return Ok(total);
                }
                destination
                    .write_all(&buffer.bytes[..count])
                    .await
                    .map_err(|_| TransferError::Io)?;
                tokio::task::yield_now().await;
            }
        })
        .await
    }
}

fn checked_size(total: u64, count: usize, maximum: u64) -> Result<u64, TransferError> {
    total
        .checked_add(count as u64)
        .filter(|total| *total <= maximum)
        .ok_or(TransferError::TooLarge)
}

async fn bounded<T>(
    duration: Duration,
    cancelled: impl Future<Output = ()>,
    transfer: impl Future<Output = Result<T, TransferError>>,
) -> Result<T, TransferError> {
    tokio::select! {
        biased;
        () = cancelled => Err(TransferError::Cancelled),
        () = tokio::time::sleep(duration) => Err(TransferError::TimedOut),
        result = transfer => result,
    }
}
