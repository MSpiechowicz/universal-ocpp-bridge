use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use time::OffsetDateTime;
use uob_application::{DeliveryOutcome, ErrorRetryClassification, TargetError, TargetErrorCode};

use super::{EventEffect, PublishPurpose, Session};
use crate::{error::check_report_task, protocol_driver::ProtocolSignal};

impl<E, P> Session<E, P>
where
    E: Serialize + Send + Sync + 'static,
    P: Clone + DeserializeOwned + Send + 'static,
{
    pub(super) async fn finish_shutdown(mut self) -> Result<(), TargetError> {
        let now = OffsetDateTime::now_utc();
        let remaining = (self.context.shutdown_deadline.into_inner() - now)
            .try_into()
            .unwrap_or(Duration::ZERO);
        let outcome = match tokio::time::timeout(remaining, self.shutdown_inner()).await {
            Ok(result) => result,
            Err(_) => Err(TargetError::new(
                TargetErrorCode::ShutdownDeadlineExceeded,
                ErrorRetryClassification::Uncertain,
                "mqtt.shutdown_deadline",
            )),
        };
        self.commands.shutdown().await;
        self.reports.shutdown().await;
        self.protocol.shutdown().await;
        outcome
    }

    async fn shutdown_inner(&mut self) -> Result<(), TargetError> {
        if !self.connected {
            if self.reset_pending {
                self.drain_reset_barrier().await?;
            }
            self.classify_lost_session();
            return self.finish_reports().await;
        }
        self.replay.clear();
        let offline = self
            .topics
            .availability_publication(&self.settings.target_instance_id, false);
        let mut availability_queued = false;
        let mut availability_acknowledged = false;
        let mut disconnect_queued = false;
        loop {
            if !availability_queued
                && self.awaiting_packet_id.len() < self.runtime.request_capacity
                && self.queue_publication(&offline, PublishPurpose::ShutdownAvailability)
            {
                availability_queued = true;
            }
            if availability_acknowledged && !disconnect_queued {
                disconnect_queued = self.client.try_disconnect().is_ok();
            }
            let has_reports = !self.reports.is_empty();
            let signal = tokio::select! {
                signal = self.protocol.next() => Some(signal),
                report = self.reports.join_next(), if has_reports => {
                    check_report_task(report.as_ref())?;
                    None
                }
            };
            let Some(signal) = signal else {
                continue;
            };
            match signal {
                Some(ProtocolSignal::Event(event)) => match self.handle_event(event)? {
                    EventEffect::ShutdownAvailabilityAcknowledged => {
                        availability_acknowledged = true;
                    }
                    EventEffect::DisconnectWritten => break,
                    EventEffect::None => {}
                },
                Some(
                    ProtocolSignal::ConnectionFailed { buffered, .. }
                    | ProtocolSignal::Reset { buffered },
                ) => {
                    self.reconcile_buffered(buffered);
                    self.connected = false;
                    self.classify_lost_session();
                    break;
                }
                Some(ProtocolSignal::Buffered { events }) => self.reconcile_buffered(events),
                None => {
                    return Err(TargetError::new(
                        TargetErrorCode::ConnectionUnavailable,
                        ErrorRetryClassification::Permanent,
                        "mqtt.protocol_driver_unavailable",
                    ));
                }
            }
        }
        self.classify_remaining_for_shutdown();
        self.finish_reports().await
    }

    async fn drain_reset_barrier(&mut self) -> Result<(), TargetError> {
        loop {
            let has_reports = !self.reports.is_empty();
            let signal = tokio::select! {
                signal = self.protocol.next() => Some(signal),
                report = self.reports.join_next(), if has_reports => {
                    check_report_task(report.as_ref())?;
                    None
                }
            };
            let Some(signal) = signal else {
                continue;
            };
            match signal {
                Some(ProtocolSignal::Event(event)) => {
                    self.handle_event(event)?;
                }
                Some(ProtocolSignal::ConnectionFailed { buffered, .. }) => {
                    self.reconcile_buffered(buffered);
                }
                Some(ProtocolSignal::Reset { buffered }) => {
                    self.reconcile_buffered(buffered);
                    self.connected = false;
                    self.reset_pending = false;
                    return Ok(());
                }
                Some(ProtocolSignal::Buffered { events }) => self.reconcile_buffered(events),
                None => {
                    return Err(TargetError::new(
                        TargetErrorCode::ConnectionUnavailable,
                        ErrorRetryClassification::Permanent,
                        "mqtt.protocol_driver_unavailable",
                    ));
                }
            }
        }
    }

    fn classify_remaining_for_shutdown(&mut self) {
        let awaiting = self.awaiting_packet_id.drain(..).collect::<Vec<_>>();
        for tracked in awaiting {
            if let PublishPurpose::Delivery(delivery_id) = tracked.purpose {
                self.spawn_report(
                    delivery_id,
                    DeliveryOutcome::RetryableFailure {
                        reason: "mqtt.shutdown_before_write".to_owned(),
                    },
                );
            }
        }
        let sent = std::mem::take(&mut self.in_flight).into_values();
        for tracked in sent {
            if let PublishPurpose::Delivery(delivery_id) = tracked.purpose {
                self.spawn_report(
                    delivery_id,
                    DeliveryOutcome::Uncertain {
                        reason: "mqtt.shutdown_before_ack".to_owned(),
                    },
                );
            }
        }
    }

    async fn finish_reports(&mut self) -> Result<(), TargetError> {
        while !self.reports.is_empty() {
            let result = self.reports.join_next().await;
            check_report_task(result.as_ref())?;
        }
        Ok(())
    }
}
