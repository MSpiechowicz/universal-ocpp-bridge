use std::{
    collections::{BTreeMap, VecDeque},
    future::poll_fn,
};

use rumqttc::{AsyncClient, Event, Incoming, Outgoing, QoS};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    task::{JoinError, JoinSet},
    time::Instant,
};
use uob_application::{
    AcknowledgementScope, DeliveryId, DeliveryOutcome, TargetContext, TargetError,
    TargetHealthState, TargetPortError,
};
use uob_contracts::CommandResult;

use crate::{
    client::create_client,
    configuration::{MqttRuntimeOptions, MqttSettings, ResolvedCredentials},
    error::{check_report_task, permanent_connection, permanent_data, permanent_refusal},
    mapping::{TopicNamespace, WirePublication},
    protocol_driver::{ProtocolDriver, ProtocolSignal},
    target::MqttTarget,
};

mod command;
mod publishing;
mod reporting;
mod shutdown;
#[cfg(test)]
mod tests;

const BROKER_ACKNOWLEDGEMENT_SCOPE: &str = "mqtt.broker_received";

pub(crate) struct Session<E, P> {
    context: TargetContext<E, P>,
    client: AsyncClient,
    protocol: ProtocolDriver,
    topics: TopicNamespace,
    settings: MqttSettings,
    runtime: MqttRuntimeOptions,
    retained_state: BTreeMap<String, WirePublication>,
    replay: VecDeque<WirePublication>,
    awaiting_packet_id: VecDeque<TrackedPurpose>,
    in_flight: BTreeMap<u16, TrackedPurpose>,
    reports: JoinSet<Result<(), TargetPortError>>,
    commands: JoinSet<CommandResult>,
    command_results: VecDeque<WirePublication>,
    connected: bool,
    connected_before: bool,
    reset_pending: bool,
    epoch: u64,
    progress_deadline: Option<Instant>,
    delivery_ingress: DeliveryIngress,
}

pub(super) enum PublishPurpose {
    Delivery(DeliveryId),
    Internal,
    ShutdownAvailability,
}

struct TrackedPurpose {
    epoch: u64,
    purpose: PublishPurpose,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DeliveryIngress {
    Open,
    Closed,
}

enum Wake<E> {
    Shutdown,
    Protocol(Option<ProtocolSignal>),
    NoProgress,
    Delivery(Option<uob_application::TargetDelivery<E>>),
    Report(Option<Result<Result<(), TargetPortError>, JoinError>>),
    Command(Option<Result<CommandResult, JoinError>>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EventEffect {
    None,
    ShutdownAvailabilityAcknowledged,
    DisconnectWritten,
}

impl<E, P> Session<E, P>
where
    E: Serialize + Send + Sync + 'static,
    P: Clone + DeserializeOwned + Send + 'static,
{
    pub(crate) fn new(
        target: MqttTarget,
        context: TargetContext<E, P>,
        credentials: Option<ResolvedCredentials>,
    ) -> Self {
        let (client, eventloop) = create_client(&target, credentials);
        let protocol = ProtocolDriver::spawn(eventloop, target.runtime.request_capacity);
        Self {
            context,
            client,
            protocol,
            topics: target.topics,
            settings: target.settings,
            runtime: target.runtime,
            retained_state: BTreeMap::new(),
            replay: VecDeque::new(),
            awaiting_packet_id: VecDeque::new(),
            in_flight: BTreeMap::new(),
            reports: JoinSet::new(),
            commands: JoinSet::new(),
            command_results: VecDeque::new(),
            connected: false,
            connected_before: false,
            reset_pending: false,
            epoch: 0,
            progress_deadline: None,
            delivery_ingress: DeliveryIngress::Open,
        }
    }

    pub(crate) async fn run(mut self) -> Result<(), TargetError> {
        let mut backoff = self.runtime.reconnect_initial_backoff;
        loop {
            self.flush_command_results();
            self.flush_replay();
            self.ensure_progress_deadline();
            let can_receive = self.can_receive_delivery();
            let has_reports = !self.reports.is_empty();
            let has_commands = !self.commands.is_empty();
            let watchdog_armed = self.connected
                && !self.reset_pending
                && self.has_protocol_work()
                && self.progress_deadline.is_some();
            let watchdog_deadline = self.progress_deadline.unwrap_or_else(Instant::now);
            let wake = {
                let deliveries = &mut self.context.deliveries;
                let shutdown = &mut self.context.shutdown;
                let protocol = &mut self.protocol;
                let reports = &mut self.reports;
                let commands = &mut self.commands;
                tokio::select! {
                    biased;
                    () = poll_fn(|cx| shutdown.as_mut().poll_shutdown(cx)) => Wake::Shutdown,
                    signal = protocol.next() => Wake::Protocol(signal),
                    () = tokio::time::sleep_until(watchdog_deadline),
                        if watchdog_armed => Wake::NoProgress,
                    delivery = poll_fn(|cx| deliveries.as_mut().poll_receive(cx)),
                        if can_receive => Wake::Delivery(delivery),
                    report = reports.join_next(), if has_reports => Wake::Report(report),
                    command = commands.join_next(), if has_commands => Wake::Command(command),
                }
            };
            match wake {
                Wake::Shutdown => return self.finish_shutdown().await,
                Wake::Delivery(Some(delivery)) => self.accept_delivery(&delivery),
                Wake::Delivery(None) => self.delivery_ingress = DeliveryIngress::Closed,
                Wake::Report(result) => check_report_task(result.as_ref())?,
                Wake::Command(result) => self.finish_command_task(result)?,
                Wake::Protocol(Some(signal)) => match signal {
                    ProtocolSignal::Event(event) => {
                        self.handle_event(event)?;
                        if self.connected {
                            backoff = self.runtime.reconnect_initial_backoff;
                        }
                    }
                    ProtocolSignal::ConnectionFailed { error, buffered } => {
                        self.reconcile_buffered(buffered);
                        self.connected = false;
                        self.progress_deadline = None;
                        self.emit_health(
                            TargetHealthState::Reconnecting,
                            "mqtt.connection_unavailable",
                        );
                        if permanent_refusal(&error) {
                            return Err(permanent_connection("mqtt.connection_refused"));
                        }
                        if self.reset_pending {
                            continue;
                        }
                        if self.wait_for_reconnect(backoff).await? {
                            return self.finish_shutdown().await;
                        }
                        self.protocol.resume().await.map_err(permanent_connection)?;
                        backoff = backoff
                            .saturating_mul(2)
                            .min(self.runtime.reconnect_maximum_backoff);
                    }
                    ProtocolSignal::Reset { buffered } => {
                        self.reconcile_buffered(buffered);
                        self.reset_pending = false;
                        self.connected = false;
                        self.progress_deadline = None;
                        self.emit_health(
                            TargetHealthState::Reconnecting,
                            "mqtt.protocol_reconnecting",
                        );
                        if self.wait_for_reconnect(backoff).await? {
                            return self.finish_shutdown().await;
                        }
                        self.protocol.resume().await.map_err(permanent_connection)?;
                        backoff = backoff
                            .saturating_mul(2)
                            .min(self.runtime.reconnect_maximum_backoff);
                    }
                    ProtocolSignal::Buffered { events } => self.reconcile_buffered(events),
                },
                Wake::Protocol(None) => {
                    return Err(permanent_connection("mqtt.protocol_driver_unavailable"));
                }
                Wake::NoProgress => {
                    self.connected = false;
                    self.reset_pending = true;
                    self.progress_deadline = None;
                    self.emit_health(TargetHealthState::Degraded, "mqtt.protocol_no_progress");
                    self.protocol.reset().await.map_err(permanent_connection)?;
                }
            }
        }
    }

    async fn wait_for_reconnect(
        &mut self,
        backoff: std::time::Duration,
    ) -> Result<bool, TargetError> {
        let shutdown = &mut self.context.shutdown;
        Ok(tokio::select! {
            () = poll_fn(|cx| shutdown.as_mut().poll_shutdown(cx)) => true,
            () = tokio::time::sleep(backoff) => false,
        })
    }

    fn can_receive_delivery(&self) -> bool {
        self.connected
            && self.delivery_ingress == DeliveryIngress::Open
            && self.awaiting_packet_id.len() < self.runtime.request_capacity
            && self.outstanding_delivery_count() < self.effective_delivery_limit()
    }

    fn effective_delivery_limit(&self) -> usize {
        self.runtime
            .maximum_in_flight_deliveries
            .min(self.context.limits.maximum_in_flight_deliveries)
    }

    fn outstanding_delivery_count(&self) -> usize {
        let awaiting = self
            .awaiting_packet_id
            .iter()
            .filter(|tracked| matches!(tracked.purpose, PublishPurpose::Delivery(_)))
            .count();
        let sent = self
            .in_flight
            .values()
            .filter(|tracked| matches!(tracked.purpose, PublishPurpose::Delivery(_)))
            .count();
        awaiting + sent + self.reports.len()
    }

    fn has_protocol_work(&self) -> bool {
        !self.awaiting_packet_id.is_empty() || !self.in_flight.is_empty() || !self.replay.is_empty()
    }

    fn ensure_progress_deadline(&mut self) {
        if self.connected && self.has_protocol_work() && self.progress_deadline.is_none() {
            self.progress_deadline = Some(Instant::now() + self.runtime.no_progress_timeout);
        }
    }

    fn note_protocol_progress(&mut self) {
        self.progress_deadline = self
            .has_protocol_work()
            .then(|| Instant::now() + self.runtime.no_progress_timeout);
    }

    fn handle_event(&mut self, event: Event) -> Result<EventEffect, TargetError> {
        match event {
            Event::Incoming(Incoming::ConnAck(connack)) => {
                let next_epoch = self
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| permanent_data("mqtt.session_epoch_exhausted"))?;
                if self.connected_before {
                    if connack.session_present {
                        self.rebind_epoch(next_epoch);
                    } else {
                        self.classify_lost_session();
                        self.client
                            .try_subscribe(self.topics.command_subscription(), QoS::AtLeastOnce)
                            .map_err(|_| permanent_data("mqtt.command_subscription_capacity"))?;
                    }
                }
                self.epoch = next_epoch;
                self.connected = true;
                self.connected_before = true;
                self.reset_pending = false;
                self.progress_deadline = None;
                self.replay.clear();
                self.replay.push_back(
                    self.topics
                        .availability_publication(&self.settings.target_instance_id, true),
                );
                self.replay.extend(self.retained_state.values().cloned());
                self.emit_health(TargetHealthState::Ready, "mqtt.broker_connected");
                Ok(EventEffect::None)
            }
            Event::Incoming(Incoming::Publish(publication)) => {
                self.handle_command(&publication);
                Ok(EventEffect::None)
            }
            Event::Outgoing(Outgoing::Publish(packet_id)) => {
                if let Some(existing) = self.in_flight.get(&packet_id) {
                    self.ensure_current_epoch(existing)?;
                    self.note_protocol_progress();
                    return Ok(EventEffect::None);
                }
                let tracked = self
                    .awaiting_packet_id
                    .pop_front()
                    .ok_or_else(|| permanent_data("mqtt.packet_identifier_without_publication"))?;
                self.ensure_current_epoch(&tracked)?;
                if self.in_flight.insert(packet_id, tracked).is_some() {
                    return Err(permanent_data("mqtt.packet_identifier_collision"));
                }
                self.note_protocol_progress();
                Ok(EventEffect::None)
            }
            Event::Incoming(Incoming::PubAck(acknowledgement)) => {
                let Some(tracked) = self.in_flight.remove(&acknowledgement.pkid) else {
                    return Err(permanent_data("mqtt.unmatched_broker_acknowledgement"));
                };
                self.ensure_current_epoch(&tracked)?;
                let effect = self.finish_acknowledgement(tracked.purpose);
                self.note_protocol_progress();
                Ok(effect)
            }
            Event::Outgoing(Outgoing::Disconnect) => Ok(EventEffect::DisconnectWritten),
            _ => Ok(EventEffect::None),
        }
    }

    fn finish_acknowledgement(&mut self, purpose: PublishPurpose) -> EventEffect {
        match purpose {
            PublishPurpose::Delivery(delivery_id) => {
                self.spawn_report(
                    delivery_id,
                    DeliveryOutcome::Acknowledged {
                        peer: self.settings.endpoint.peer_name(),
                        scope: AcknowledgementScope(BROKER_ACKNOWLEDGEMENT_SCOPE.to_owned()),
                    },
                );
                EventEffect::None
            }
            PublishPurpose::ShutdownAvailability => EventEffect::ShutdownAvailabilityAcknowledged,
            PublishPurpose::Internal => EventEffect::None,
        }
    }

    fn ensure_current_epoch(&self, tracked: &TrackedPurpose) -> Result<(), TargetError> {
        if tracked.epoch == self.epoch {
            Ok(())
        } else {
            Err(permanent_data("mqtt.stale_session_correlation"))
        }
    }

    fn rebind_epoch(&mut self, epoch: u64) {
        for tracked in &mut self.awaiting_packet_id {
            tracked.epoch = epoch;
        }
        for tracked in self.in_flight.values_mut() {
            tracked.epoch = epoch;
        }
    }

    fn reconcile_buffered(&mut self, events: Vec<Event>) {
        for event in events {
            match event {
                Event::Outgoing(Outgoing::Publish(packet_id)) => {
                    if self.in_flight.contains_key(&packet_id) {
                        continue;
                    }
                    let Some(tracked) = self.awaiting_packet_id.pop_front() else {
                        self.emit_health(TargetHealthState::Degraded, "mqtt.stale_buffered_event");
                        continue;
                    };
                    if self.in_flight.insert(packet_id, tracked).is_some() {
                        self.emit_health(TargetHealthState::Degraded, "mqtt.stale_buffered_event");
                    }
                }
                Event::Incoming(Incoming::PubAck(acknowledgement)) => {
                    let Some(tracked) = self.in_flight.remove(&acknowledgement.pkid) else {
                        self.emit_health(TargetHealthState::Degraded, "mqtt.stale_buffered_event");
                        continue;
                    };
                    self.finish_acknowledgement(tracked.purpose);
                }
                _ => {}
            }
        }
    }

    fn classify_lost_session(&mut self) {
        // The client moves both written in-flight publishes and accepted request-channel
        // publishes into its pending queue on a network error, then invalidates that old broker
        // session when a fresh ConnAck arrives. Keeping our FIFO would therefore correlate a later
        // publication with a cancelled request. Drain it in the same ConnAck turn.
        let awaiting = self.awaiting_packet_id.drain(..).collect::<Vec<_>>();
        let sent = std::mem::take(&mut self.in_flight).into_values();
        // Missing Outgoing::Publish does not prove that no bytes reached the peer: rumqttc queues
        // that event before its write but only yields it after the write and flush complete.
        for tracked in awaiting.into_iter().chain(sent) {
            if let PublishPurpose::Delivery(delivery_id) = tracked.purpose {
                self.spawn_report(
                    delivery_id,
                    DeliveryOutcome::Uncertain {
                        reason: "mqtt.session_lost_before_ack".to_owned(),
                    },
                );
            }
        }
        self.emit_health(TargetHealthState::Degraded, "mqtt.session_state_lost");
    }
}
