use tokio::sync::watch;

#[derive(Clone, Debug)]
pub struct CancellationHandle {
    sender: watch::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    receiver: watch::Receiver<bool>,
}

#[must_use]
pub fn cancellation_pair() -> (CancellationHandle, CancellationToken) {
    let (sender, receiver) = watch::channel(false);
    (
        CancellationHandle { sender },
        CancellationToken { receiver },
    )
}

impl CancellationHandle {
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

impl CancellationToken {
    pub(super) fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub(super) async fn cancelled(&mut self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            if self.receiver.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}
