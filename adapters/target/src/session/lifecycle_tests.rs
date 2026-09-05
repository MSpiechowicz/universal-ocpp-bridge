use std::{future::pending, time::Duration};

use tokio::sync::oneshot;

use super::{TargetSessionError, TargetSessionTask};

fn stalled() -> (TargetSessionTask, oneshot::Receiver<()>) {
    let (alive, dropped) = oneshot::channel::<()>();
    let (stop, _stopped) = oneshot::channel();
    let join = tokio::spawn(async move {
        let _alive = alive;
        pending().await
    });
    (
        TargetSessionTask {
            shutdown: Some(stop),
            join: Some(join),
        },
        dropped,
    )
}

#[tokio::test]
async fn cancelling_wait_or_shutdown_does_not_detach_reconnected_workers() {
    for _ in 0..8 {
        for shutting_down in [false, true] {
            let (task, dropped) = stalled();
            let operation = async move {
                if shutting_down {
                    task.shutdown(Duration::from_secs(60)).await
                } else {
                    task.wait().await
                }
            };
            assert!(
                tokio::time::timeout(Duration::from_millis(5), operation)
                    .await
                    .is_err()
            );
            assert!(
                tokio::time::timeout(Duration::from_secs(1), dropped)
                    .await
                    .unwrap()
                    .is_err()
            );
        }
    }
}

#[tokio::test]
async fn deadline_joins_the_cancelled_worker_before_returning() {
    let (task, mut dropped) = stalled();
    assert!(matches!(
        task.shutdown(Duration::from_millis(5)).await,
        Err(TargetSessionError::ShutdownDeadlineExceeded)
    ));
    assert!(matches!(
        dropped.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
}
