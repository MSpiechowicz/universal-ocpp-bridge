use rumqttc::{ConnectionError, Event, EventLoop};
use tokio::{sync::mpsc, task::JoinHandle};

/// One owned `EventLoop` poll is kept alive until it yields or an explicit reset cancels it.
pub(crate) struct ProtocolDriver {
    commands: mpsc::Sender<DriverCommand>,
    signals: mpsc::Receiver<ProtocolSignal>,
    task: JoinHandle<()>,
}

pub(crate) enum ProtocolSignal {
    Event(Event),
    ConnectionFailed {
        error: ConnectionError,
        buffered: Vec<Event>,
    },
    Reset {
        buffered: Vec<Event>,
    },
    Buffered {
        events: Vec<Event>,
    },
}

enum DriverCommand {
    Reset,
    Resume,
}

impl ProtocolDriver {
    pub(crate) fn spawn(eventloop: EventLoop, capacity: usize) -> Self {
        let (command_tx, command_rx) = mpsc::channel(1);
        let (signal_tx, signal_rx) = mpsc::channel(capacity.max(1));
        let task = tokio::spawn(run_driver(eventloop, command_rx, signal_tx));
        Self {
            commands: command_tx,
            signals: signal_rx,
            task,
        }
    }

    pub(crate) async fn next(&mut self) -> Option<ProtocolSignal> {
        self.signals.recv().await
    }

    pub(crate) async fn reset(&self) -> Result<(), &'static str> {
        self.commands
            .send(DriverCommand::Reset)
            .await
            .map_err(|_| "mqtt.protocol_driver_unavailable")
    }

    pub(crate) async fn resume(&self) -> Result<(), &'static str> {
        self.commands
            .send(DriverCommand::Resume)
            .await
            .map_err(|_| "mqtt.protocol_driver_unavailable")
    }
}

impl Drop for ProtocolDriver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run_driver(
    mut eventloop: EventLoop,
    mut commands: mpsc::Receiver<DriverCommand>,
    signals: mpsc::Sender<ProtocolSignal>,
) {
    let mut active = true;
    loop {
        if !active {
            match commands.recv().await {
                Some(DriverCommand::Resume) => {
                    let events = prepare_resume(&mut eventloop);
                    if !events.is_empty()
                        && signals
                            .send(ProtocolSignal::Buffered { events })
                            .await
                            .is_err()
                    {
                        return;
                    }
                    active = true;
                }
                Some(DriverCommand::Reset) => {
                    eventloop.clean();
                    if signals
                        .send(ProtocolSignal::Reset {
                            buffered: drain_buffered(&mut eventloop),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                None => return,
            }
            continue;
        }

        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(DriverCommand::Reset | DriverCommand::Resume) => {
                    // Cancelling EventLoop::poll can leave an Outgoing event queued before its
                    // socket write completed. clean() preserves the matching request for MQTT
                    // session recovery; the buffered event is reconciled by the session first.
                    // An unexpected Resume while active takes the same safe reset path.
                    eventloop.clean();
                    active = false;
                    if signals.send(ProtocolSignal::Reset {
                        buffered: drain_buffered(&mut eventloop),
                    }).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
            result = eventloop.poll() => match result {
                Ok(event) => {
                    if signals.send(ProtocolSignal::Event(event)).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    active = false;
                    if signals.send(ProtocolSignal::ConnectionFailed {
                        error,
                        buffered: drain_buffered(&mut eventloop),
                    }).await.is_err() {
                        return;
                    }
                }
            },
        }
    }
}

fn drain_buffered(eventloop: &mut EventLoop) -> Vec<Event> {
    eventloop.state.events.drain(..).collect()
}

fn prepare_resume(eventloop: &mut EventLoop) -> Vec<Event> {
    // Sweep requests admitted after a poll error but before its signal reached Session. This puts
    // rumqttc and Session back in lockstep before either a resumed or fresh broker session starts.
    eventloop.clean();
    drain_buffered(eventloop)
}

#[cfg(test)]
mod tests {
    use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, Outgoing, PubAck, PublishOptions};

    use super::{drain_buffered, prepare_resume};

    #[test]
    fn resume_sweeps_a_request_admitted_after_the_first_clean() {
        let options = MqttOptions::new("client-a", "localhost");
        let (client, mut eventloop) = AsyncClient::builder(options).capacity(2).build();
        eventloop.clean();
        client
            .try_publish("topic", b"body", PublishOptions::at_least_once())
            .expect("queue late request");

        let buffered = prepare_resume(&mut eventloop);

        assert!(buffered.is_empty());
        assert_eq!(eventloop.pending_len(), 1);
    }

    #[test]
    fn buffered_outgoing_and_ack_are_drained_in_fifo_order() {
        let options = MqttOptions::new("client-a", "localhost");
        let (_client, mut eventloop) = AsyncClient::builder(options).capacity(1).build();
        eventloop
            .state
            .events
            .push_back(Event::Outgoing(Outgoing::Publish(7)));
        eventloop
            .state
            .events
            .push_back(Event::Incoming(Incoming::PubAck(PubAck::new(7))));

        let buffered = drain_buffered(&mut eventloop);

        assert_eq!(
            buffered,
            vec![
                Event::Outgoing(Outgoing::Publish(7)),
                Event::Incoming(Incoming::PubAck(PubAck::new(7))),
            ]
        );
        assert!(eventloop.state.events.is_empty());
    }
}
