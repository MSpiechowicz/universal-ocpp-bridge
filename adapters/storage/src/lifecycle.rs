use std::{
    sync::{
        Mutex, PoisonError,
        mpsc::{SyncSender, TrySendError},
    },
    thread::JoinHandle,
    time::Duration,
};

use uob_application::{StorageError, StorageErrorCode};

/// Shared admission and ownership: clones cannot enqueue after shutdown begins.
pub(crate) struct Worker<T> {
    sender: Mutex<Option<SyncSender<T>>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl<T> Worker<T> {
    pub(crate) fn new(sender: SyncSender<T>, join: JoinHandle<()>) -> Self {
        Self {
            sender: Mutex::new(Some(sender)),
            join: Mutex::new(Some(join)),
        }
    }

    pub(crate) fn try_send(&self, request: T) -> Result<(), TrySendError<T>> {
        let sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
        match sender.as_ref() {
            Some(sender) => sender.try_send(request),
            None => Err(TrySendError::Disconnected(request)),
        }
    }

    pub(crate) async fn shutdown(&self, deadline: Duration) -> Result<(), StorageError> {
        self.sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let wait = async {
            loop {
                {
                    let mut join = self.join.lock().unwrap_or_else(PoisonError::into_inner);
                    if join.as_ref().is_none_or(JoinHandle::is_finished) {
                        return match join.take() {
                            Some(join) => join.join().map_err(|_| {
                                StorageError::new(
                                    StorageErrorCode::Unavailable,
                                    "storage worker failed during shutdown",
                                )
                            }),
                            None => Ok(()),
                        };
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        tokio::time::timeout(deadline, wait)
            .await
            .unwrap_or_else(|_| {
                Err(StorageError::new(
                    StorageErrorCode::Unavailable,
                    "storage shutdown deadline exceeded; recovery required",
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[tokio::test]
    async fn stalled_worker_retains_ownership_and_rejects_new_work_after_deadline() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let (release, stalled) = mpsc::channel();
        let join = std::thread::spawn(move || {
            stalled.recv().unwrap();
            assert_eq!(receiver.into_iter().collect::<Vec<_>>(), vec![1, 2]);
        });
        let worker = Worker::new(sender, join);
        worker.try_send(1).unwrap();
        worker.try_send(2).unwrap();
        assert!(worker.shutdown(Duration::from_millis(5)).await.is_err());
        assert!(matches!(
            worker.try_send(3),
            Err(TrySendError::Disconnected(3))
        ));
        assert!(worker.join.lock().unwrap().is_some());
        release.send(()).unwrap();
        worker.shutdown(Duration::from_secs(1)).await.unwrap();
        assert!(worker.join.lock().unwrap().is_none());
    }
}
