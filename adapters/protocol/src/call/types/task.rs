use std::time::Duration;

use tokio::time::timeout;

use super::CallSessionTask;

impl CallSessionTask {
    /// Waits for peer disconnection or another terminal socket condition.
    ///
    /// # Errors
    ///
    /// Reports an unavailable, cancelled, or panicked session task.
    pub async fn wait(mut self) -> Result<(), &'static str> {
        let Some(join) = self.join.as_mut() else {
            return Err("session task unavailable");
        };
        join.await.map_err(|_| "session task failed")
    }

    /// Requests graceful socket close within a caller-supplied deadline.
    ///
    /// # Errors
    ///
    /// Reports an unavailable, failed, or deadline-exceeding session task.
    pub async fn shutdown(mut self, deadline: Duration) -> Result<(), &'static str> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let join = self.join.as_mut().ok_or("session task unavailable")?;
        match timeout(deadline, &mut *join).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("session task failed"),
            Err(_) => {
                join.abort();
                let _ = join.await;
                Err("session shutdown deadline exceeded")
            }
        }
    }
}

impl Drop for CallSessionTask {
    fn drop(&mut self) {
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}
