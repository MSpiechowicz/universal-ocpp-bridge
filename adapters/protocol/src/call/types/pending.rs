use super::{PendingCall, RuntimeReservation, SessionCallOutcome};
use crate::TransmissionUncertainReason;

impl PendingCall {
    /// Waits for correlation, timeout, or conservative transport classification.
    pub async fn receive(self) -> SessionCallOutcome {
        self.receive_guarded().await.0
    }

    /// Native query decoding owns the inbound reservation until sanitization and commit finish.
    pub(crate) async fn receive_guarded(self) -> (SessionCallOutcome, Option<RuntimeReservation>) {
        let outcome = self
            .receiver
            .await
            .unwrap_or(SessionCallOutcome::TransmissionUncertain {
                reason: TransmissionUncertainReason::SessionStopped,
                correlation_id: self.correlation_id,
            });
        let reservation = match self.response_reservation {
            Some(receiver) => receiver.await.ok(),
            None => None,
        };
        (outcome, reservation)
    }
}
