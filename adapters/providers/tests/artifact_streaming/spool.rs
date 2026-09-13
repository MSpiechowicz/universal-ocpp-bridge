use super::*;
use std::{fs, path::PathBuf};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-artifact-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn empty(&self) {
        assert_eq!(fs::read_dir(&self.0).unwrap().count(), 0);
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn large_spool_roundtrip_is_exact_unlinked_and_budgeted() {
    let directory = Directory::new();
    let budget = budget();
    let bytes = 129 * 1024 * 1024;
    let transfers = transfers(&budget, bytes, Duration::from_secs(60));
    let artifact = transfers
        .receive(source(bytes), &directory.0, pending())
        .await
        .unwrap();
    assert_eq!(artifact.len(), bytes);
    assert!(!artifact.is_empty());
    directory.empty();
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    let mut writer = PatternWriter {
        total: 0,
        budget: budget.clone(),
    };
    assert_eq!(
        transfers.send(artifact, &mut writer, pending()).await,
        Ok(bytes)
    );
    assert_eq!(writer.total, bytes);
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    directory.empty();
}

#[tokio::test]
async fn failed_or_cancelled_spool_never_publishes_partial_artifacts() {
    let directory = Directory::new();
    let budget = budget();
    let transfers = transfers(&budget, 4, Duration::from_secs(1));
    assert_eq!(
        transfers
            .receive(&b"abcde"[..], &directory.0, pending())
            .await
            .unwrap_err(),
        TransferError::TooLarge
    );
    directory.empty();
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    assert_eq!(
        transfers
            .receive(tokio::io::empty(), &directory.0.join("missing"), pending())
            .await
            .unwrap_err(),
        TransferError::Io
    );
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
    let (source, _peer) = tokio::io::duplex(1);
    assert_eq!(
        transfers
            .receive(
                source,
                &directory.0,
                tokio::time::sleep(Duration::from_millis(20))
            )
            .await
            .unwrap_err(),
        TransferError::Cancelled
    );
    // A blocking file operation may still own admission at cancellation; it must release it
    // on completion rather than detaching unaccounted memory or a temporary-file handle.
    wait_for_cleanup(&budget).await;
    directory.empty();
}

#[tokio::test]
async fn send_rechecks_size_and_cleans_up_on_failed_destination() {
    let directory = Directory::new();
    let budget = budget();
    let large = transfers(&budget, 10, Duration::from_secs(1));
    let small = transfers(&budget, 1, Duration::from_secs(1));
    let artifact = large
        .receive(&b"hello"[..], &directory.0, pending())
        .await
        .unwrap();
    assert_eq!(
        small.send(artifact, tokio::io::sink(), pending()).await,
        Err(TransferError::TooLarge)
    );
    let artifact = large
        .receive(&b"hello"[..], &directory.0, pending())
        .await
        .unwrap();
    assert_eq!(
        large.send(artifact, FailedWriter(false), pending()).await,
        Err(TransferError::Io)
    );
    wait_for_cleanup(&budget).await;
    directory.empty();
}

async fn wait_for_cleanup(budget: &RuntimeResourceBudget) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while budget.snapshot().queued_payload_bytes != 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(budget.snapshot().queues.multipart_assemblies, 0);
}
