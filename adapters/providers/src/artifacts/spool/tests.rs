use super::*;
use crate::artifacts::TransferLimits;
use std::{os::fd::AsRawFd, time::Duration};
use uob_application::{RuntimeResourceBudget, RuntimeResourceLimits};

#[tokio::test]
async fn abandoned_file_job_retains_its_reservation_and_closes_its_unlinked_file() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let transfers = ArtifactTransfers::new(
        budget.clone(),
        TransferLimits {
            buffer_bytes: 1024,
            maximum_artifact_bytes: 4096,
            timeout: Duration::from_secs(1),
        },
    )
    .unwrap();
    let buffer = transfers.reserve().unwrap();
    let directory =
        std::env::temp_dir().join(format!("uob-blocking-transfer-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let worker_directory = directory.clone();
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        blocking(move || {
            let state = FileBuffer {
                file: temporary_file(&worker_directory)?,
                buffer,
            };
            started.send(state.file.as_raw_fd()).unwrap();
            released.recv().unwrap();
            Ok(state)
        })
        .await
    });
    let fd = started_rx.await.unwrap();
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    assert_eq!(budget.snapshot().queued_payload_bytes, 1024);
    assert_eq!(budget.snapshot().queues.multipart_assemblies, 1);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while budget.snapshot().queued_payload_bytes != 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    // On Linux, also prove the file handle was closed, not merely its directory entry removed.
    if cfg!(target_os = "linux") {
        assert!(
            !std::fs::read_link(format!("/proc/self/fd/{fd}"))
                .is_ok_and(|path| path.starts_with(&directory))
        );
    }
    assert_eq!(budget.snapshot().queues.multipart_assemblies, 0);
    std::fs::remove_dir(directory).unwrap();
}
