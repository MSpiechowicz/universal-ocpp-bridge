use std::{collections::BTreeSet, convert::Infallible, sync::Arc, time::Duration};

use tokio::sync::Barrier;
use uob_application::{
    RuntimeResourceBudget, RuntimeResourceLimits, SizedStationOutput, StationEffects, StationInput,
    StationStateMachine,
};
use uob_protocol_adapter::{StationSendError, spawn_station};

#[derive(Default)]
struct TestMachine;

impl StationStateMachine for TestMachine {
    type Observation = String;
    type Command = String;
    type ProtocolResponse = String;
    type Transition = String;
    type Dispatch = String;
    type Error = Infallible;

    fn apply(
        &mut self,
        input: StationInput<Self::Observation, Self::Command, Self::ProtocolResponse>,
    ) -> Result<StationEffects<Self::Transition, Self::Dispatch>, Self::Error> {
        let effects = match input {
            StationInput::Observation(value) => StationEffects {
                transitions: vec![sized(format!("observed:{value}"))],
                dispatches: Vec::new(),
            },
            StationInput::Command(value) => StationEffects {
                transitions: vec![sized(format!("command:{value}"))],
                dispatches: vec![sized(format!("dispatch:{value}"))],
            },
            StationInput::ProtocolResponse(value) => StationEffects {
                transitions: vec![sized(format!("response:{value}"))],
                dispatches: Vec::new(),
            },
        };
        Ok(effects)
    }
}

fn sized(value: String) -> SizedStationOutput<String> {
    let encoded_bytes = value.len();
    SizedStationOutput::new(value, encoded_bytes)
}

fn budget() -> Arc<RuntimeResourceBudget> {
    Arc::new(RuntimeResourceBudget::new(RuntimeResourceLimits::default()).expect("runtime budget"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_station_response_does_not_block_other_stations_or_keepalives() {
    let budget = budget();
    let (station_a, mut outputs_a, task_a) =
        spawn_station(TestMachine, Arc::clone(&budget), 8, 8).expect("station A");
    let (station_b, mut outputs_b, task_b) =
        spawn_station(TestMachine, Arc::clone(&budget), 8, 8).expect("station B");

    station_a
        .try_command("start-a".into(), 7)
        .expect("A command");
    station_b
        .try_command("start-b".into(), 7)
        .expect("B command");
    assert_eq!(
        outputs_a
            .dispatches
            .receive()
            .await
            .expect("A dispatch")
            .value,
        "dispatch:start-a"
    );
    assert_eq!(
        outputs_b
            .dispatches
            .receive()
            .await
            .expect("B dispatch")
            .value,
        "dispatch:start-b"
    );

    station_b
        .try_protocol_response("accepted-b".into(), 10)
        .expect("B response");
    station_a
        .try_observe("heartbeat-a".into(), 11)
        .expect("A keepalive");

    let a_command = outputs_a
        .transitions
        .receive()
        .await
        .expect("A command transition");
    let a_keepalive = outputs_a
        .transitions
        .receive()
        .await
        .expect("A keepalive transition");
    let b_command = outputs_b
        .transitions
        .receive()
        .await
        .expect("B command transition");
    let b_response = outputs_b
        .transitions
        .receive()
        .await
        .expect("B response transition");

    assert_eq!(
        (a_command.input_sequence, a_command.value.as_str()),
        (0, "command:start-a")
    );
    assert_eq!(
        (a_keepalive.input_sequence, a_keepalive.value.as_str()),
        (1, "observed:heartbeat-a")
    );
    assert_eq!(
        (b_command.input_sequence, b_command.value.as_str()),
        (0, "command:start-b")
    );
    assert_eq!(
        (b_response.input_sequence, b_response.value.as_str()),
        (1, "response:accepted-b")
    );

    task_a
        .shutdown(Duration::from_secs(1))
        .await
        .expect("stop A");
    task_b
        .shutdown(Duration::from_secs(1))
        .await
        .expect("stop B");
    assert_eq!(budget.snapshot().connected_stations, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_inputs_are_serialized_and_shutdown_releases_all_capacity() {
    let budget = budget();
    let (station, mut outputs, task) =
        spawn_station(TestMachine, Arc::clone(&budget), 32, 32).expect("station");
    let barrier = Arc::new(Barrier::new(17));
    let mut submitters = Vec::new();

    for index in 0..16 {
        let sender = station.clone();
        let barrier = Arc::clone(&barrier);
        submitters.push(tokio::spawn(async move {
            barrier.wait().await;
            if index % 2 == 0 {
                sender.try_command(format!("command-{index}"), 10)
            } else {
                sender.try_observe(format!("observation-{index}"), 14)
            }
        }));
    }
    barrier.wait().await;
    for submitter in submitters {
        submitter
            .await
            .expect("submitter task")
            .expect("input admitted");
    }

    let mut values = BTreeSet::new();
    for sequence in 0..16 {
        let output = outputs.transitions.receive().await.expect("ordered output");
        assert_eq!(output.input_sequence, sequence);
        assert!(values.insert(output.value));
    }
    assert_eq!(values.len(), 16);

    task.shutdown(Duration::from_secs(1))
        .await
        .expect("shutdown");
    assert!(matches!(
        station.try_command("late".into(), 4),
        Err(StationSendError::Closed(command)) if command == "late"
    ));
    drop(outputs);
    assert_eq!(budget.snapshot().connected_stations, 0);
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}

#[tokio::test]
async fn full_mailbox_rejects_work_without_leaking_its_reservation() {
    let budget = budget();
    let (station, outputs, task) =
        spawn_station(TestMachine, Arc::clone(&budget), 2, 4).expect("station");

    station.try_command("first".into(), 5).expect("first");
    station.try_command("second".into(), 6).expect("second");
    assert!(matches!(
        station.try_command("third".into(), 5),
        Err(StationSendError::Full(command)) if command == "third"
    ));
    assert_eq!(budget.snapshot().queued_payload_bytes, 11);

    task.shutdown(Duration::from_secs(1))
        .await
        .expect("shutdown");
    drop(outputs);
    assert_eq!(budget.snapshot().connected_stations, 0);
    assert_eq!(budget.snapshot().queued_payload_bytes, 0);
}
