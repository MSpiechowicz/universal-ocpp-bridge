use std::{
    fs::{File, OpenOptions},
    future::Future,
    io::{Read, Seek, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{ArtifactTransfers, Buffer, TransferError, bounded, checked_size};

/// Private, unlinked Unix temporary file. Dropping the artifact releases its storage.
/// This is ephemeral transfer staging, not a durable or integrity-verified artifact store.
#[derive(Debug)]
pub struct TemporaryArtifact {
    file: File,
    bytes: u64,
}

impl TemporaryArtifact {
    /// Exact completed byte count, independent of any peer-supplied content length.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes == 0
    }
}

struct FileBuffer {
    file: File,
    buffer: Buffer,
}

impl ArtifactTransfers {
    /// Receive a diagnostic upload or firmware download into a private temporary file.
    /// The directory must be a trusted, disk-budgeted spool on the intended filesystem.
    /// No artifact path or partially completed artifact is published. Files are unlinked
    /// immediately after creation, before reading any peer bytes.
    ///
    /// Blocking file work owns its file, buffer and reservation. Cancellation never waits
    /// for a kernel I/O operation: that job retains admission until it finishes, then cleans
    /// up even if its awaiting future has been dropped. No further job is scheduled.
    ///
    /// # Errors
    /// Returns a sanitized capacity, size, I/O, cancellation or absolute-deadline failure.
    pub async fn receive<R, C>(
        &self,
        mut source: R,
        directory: &Path,
        cancelled: C,
    ) -> Result<TemporaryArtifact, TransferError>
    where
        R: AsyncRead + Unpin,
        C: Future<Output = ()>,
    {
        bounded(self.limits.timeout, cancelled, async {
            let buffer = self.reserve()?;
            let directory = directory.to_owned();
            let mut state = blocking(move || {
                Ok(FileBuffer {
                    file: temporary_file(&directory)?,
                    buffer,
                })
            })
            .await?;
            let mut total = 0;
            loop {
                let count = source
                    .read(&mut state.buffer.bytes)
                    .await
                    .map_err(|_| TransferError::Io)?;
                total = checked_size(total, count, self.limits.maximum_artifact_bytes)?;
                if count == 0 {
                    // Unbuffered std File writes have completed. This ephemeral spool makes no
                    // durability claim; a persistent artifact provider must separately fsync.
                    return Ok(TemporaryArtifact {
                        file: state.file,
                        bytes: total,
                    });
                }
                state = blocking(move || {
                    state.file.write_all(&state.buffer.bytes[..count])?;
                    Ok(state)
                })
                .await?;
                tokio::task::yield_now().await;
            }
        })
        .await
    }

    /// Stream a completed spool to a provider transport, consuming the temporary artifact.
    /// Completion means byte transfer and flush only, never device acceptance or installation.
    ///
    /// # Errors
    /// Returns a sanitized capacity, size, I/O, cancellation or absolute-deadline failure.
    pub async fn send<W, C>(
        &self,
        artifact: TemporaryArtifact,
        mut destination: W,
        cancelled: C,
    ) -> Result<u64, TransferError>
    where
        W: AsyncWrite + Unpin,
        C: Future<Output = ()>,
    {
        bounded(self.limits.timeout, cancelled, async {
            if artifact.bytes > self.limits.maximum_artifact_bytes {
                return Err(TransferError::TooLarge);
            }
            let mut state = FileBuffer {
                file: artifact.file,
                buffer: self.reserve()?,
            };
            state = blocking(move || {
                state.file.rewind()?;
                Ok(state)
            })
            .await?;
            let mut total = 0;
            loop {
                let (next, count) = blocking(move || {
                    let count = state.file.read(&mut state.buffer.bytes)?;
                    Ok((state, count))
                })
                .await?;
                state = next;
                total = checked_size(total, count, self.limits.maximum_artifact_bytes)?;
                if count == 0 {
                    if total != artifact.bytes {
                        return Err(TransferError::Io);
                    }
                    destination.flush().await.map_err(|_| TransferError::Io)?;
                    return Ok(total);
                }
                destination
                    .write_all(&state.buffer.bytes[..count])
                    .await
                    .map_err(|_| TransferError::Io)?;
                tokio::task::yield_now().await;
            }
        })
        .await
    }
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, TransferError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| TransferError::Io)?
        .map_err(|_| TransferError::Io)
}

fn temporary_file(directory: &Path) -> std::io::Result<File> {
    let path: PathBuf = directory.join(format!(".uob-transfer-{}", uuid::Uuid::new_v4()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = std::fs::remove_file(&path) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

#[cfg(test)]
mod tests;
