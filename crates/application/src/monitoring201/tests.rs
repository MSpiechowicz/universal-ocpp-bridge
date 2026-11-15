use super::{
    MonitoringEventDecision201, MonitoringEventGate201, MonitoringEventLimits201,
    NotifyEventObservation201,
};
use crate::{RuntimeResourceBudget, RuntimeResourceLimits, record_monitoring_event_shed_201};
use std::time::Duration;
use uob_contracts::{
    DeviceComponent201, DeviceEvse201, DeviceVariable201, EventNotification201, EventTrigger201,
    MONITORING_EVENT_EVIDENCE_LIMIT_201, MONITORING_EVENTS_PER_RECORD_LIMIT_201,
    MonitoringEvent201, MonitoringEventOrdering201, MonitoringEventRecord201, UtcTimestamp,
};

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}

fn event(id: i32, actual_value: String) -> MonitoringEvent201 {
    MonitoringEvent201 {
        event_id: id,
        timestamp: at(0),
        trigger: EventTrigger201::Alerting,
        cause: None,
        actual_value,
        tech_code: None,
        tech_info: None,
        cleared: None,
        transaction_id: None,
        variable_monitoring_id: Some(7),
        notification: EventNotification201::CustomMonitor,
        component: DeviceComponent201 {
            name: "EVSE".to_owned(),
            instance: None,
            evse: Some(DeviceEvse201 {
                id: 1,
                connector_id: None,
            }),
        },
        variable: DeviceVariable201 {
            name: "Power".to_owned(),
            instance: None,
        },
    }
}

fn call(seq_no: u32, tbc: bool, events: usize) -> NotifyEventObservation201 {
    NotifyEventObservation201 {
        generated_at: at(0),
        seq_no,
        tbc,
        events: (0..events)
            .map(|index| event(i32::try_from(index).unwrap(), "7500.5".to_owned()))
            .collect(),
    }
}

fn limits(records: u32, bytes: usize) -> MonitoringEventLimits201 {
    MonitoringEventLimits201 {
        window: Duration::from_secs(10),
        records,
        bytes,
    }
}

/// Admits and commits a call the test expects to be journaled.
fn recorded(
    gate: &mut MonitoringEventGate201,
    observation: NotifyEventObservation201,
    now: UtcTimestamp,
) -> MonitoringEventRecord201 {
    match gate.admit(observation, now) {
        MonitoringEventDecision201::Record { record, receipt } => {
            gate.committed(receipt);
            record
        }
        MonitoringEventDecision201::Shed { .. } => panic!("expected a journal record"),
    }
}

fn ordering(
    gate: &mut MonitoringEventGate201,
    seq_no: u32,
    tbc: bool,
) -> MonitoringEventOrdering201 {
    recorded(gate, call(seq_no, tbc, 1), at(0)).ordering
}

fn encoded_len(record: &MonitoringEventRecord201) -> usize {
    serde_json::to_vec(record).unwrap().len()
}

#[test]
fn a_series_starting_at_zero_and_its_continuations_are_in_sequence() {
    let mut gate = MonitoringEventGate201::default();

    assert_eq!(
        ordering(&mut gate, 0, true),
        MonitoringEventOrdering201::InSequence
    );
    assert_eq!(
        ordering(&mut gate, 1, true),
        MonitoringEventOrdering201::InSequence
    );
    assert_eq!(
        ordering(&mut gate, 2, false),
        MonitoringEventOrdering201::InSequence
    );
    // The finished series is followed by a new one starting at zero.
    assert_eq!(
        ordering(&mut gate, 0, false),
        MonitoringEventOrdering201::InSequence
    );
}

#[test]
fn zero_in_the_middle_of_a_series_is_a_restart() {
    let mut gate = MonitoringEventGate201::default();
    ordering(&mut gate, 0, true);
    ordering(&mut gate, 1, true);

    assert_eq!(
        ordering(&mut gate, 0, true),
        MonitoringEventOrdering201::Restarted
    );
    // The restarted series carries on from its own position.
    assert_eq!(
        ordering(&mut gate, 1, false),
        MonitoringEventOrdering201::InSequence
    );
}

#[test]
fn any_other_unexpected_number_reports_the_expected_one() {
    let mut gate = MonitoringEventGate201::default();
    assert_eq!(
        ordering(&mut gate, 3, true),
        MonitoringEventOrdering201::Gap { expected: 0 },
        "a connection never saw the start of this series"
    );
    assert_eq!(
        ordering(&mut gate, 6, true),
        MonitoringEventOrdering201::Gap { expected: 4 }
    );
    assert_eq!(
        ordering(&mut gate, 7, false),
        MonitoringEventOrdering201::InSequence
    );
    assert_eq!(
        ordering(&mut gate, 2, false),
        MonitoringEventOrdering201::Gap { expected: 0 },
        "after a finished series only zero starts the next one"
    );
}

#[test]
fn a_failed_write_leaves_the_retry_judged_like_the_first_attempt() {
    let mut gate = MonitoringEventGate201::default();
    ordering(&mut gate, 0, true);

    let first = gate.admit(call(1, true, 1), at(0));
    assert!(matches!(first, MonitoringEventDecision201::Record { .. }));
    drop(first);

    let retry = recorded(&mut gate, call(1, true, 1), at(0));
    assert_eq!(retry.ordering, MonitoringEventOrdering201::InSequence);
}

#[test]
fn shed_calls_advance_the_ordering_so_no_gap_is_reported_afterwards() {
    let mut gate = MonitoringEventGate201::new(limits(1, usize::MAX));
    ordering(&mut gate, 0, true);

    assert!(matches!(
        gate.admit(call(1, true, 1), at(1)),
        MonitoringEventDecision201::Shed { .. }
    ));
    assert!(matches!(
        gate.admit(call(2, true, 1), at(2)),
        MonitoringEventDecision201::Shed { .. }
    ));

    let next = recorded(&mut gate, call(3, false, 1), at(11));
    assert_eq!(next.ordering, MonitoringEventOrdering201::InSequence);
}

#[test]
fn more_than_thirty_two_events_keep_the_first_thirty_two_and_count_the_rest() {
    let mut gate = MonitoringEventGate201::default();

    let record = recorded(&mut gate, call(0, false, 40), at(0));

    assert_eq!(record.events.len(), MONITORING_EVENTS_PER_RECORD_LIMIT_201);
    assert_eq!(record.events_omitted, 8);
    let ids: Vec<i32> = record.events.iter().map(|event| event.event_id).collect();
    assert_eq!(ids, (0..32).collect::<Vec<_>>(), "native order is kept");
}

#[test]
fn oversized_evidence_is_trimmed_to_the_bound_without_losing_the_count() {
    let mut gate = MonitoringEventGate201::default();
    let mut observation = call(0, false, 0);
    observation.events = (0..30).map(|id| event(id, "x".repeat(2500))).collect();

    let record = recorded(&mut gate, observation, at(0));

    assert!(encoded_len(&record) <= MONITORING_EVENT_EVIDENCE_LIMIT_201);
    assert!(record.events.len() < 30 && !record.events.is_empty());
    assert_eq!(
        usize::try_from(record.events_omitted).unwrap() + record.events.len(),
        30,
        "kept plus omitted is everything received"
    );
    assert_eq!(record.events[0].event_id, 0);
}

#[test]
fn events_within_the_bounds_are_kept_whole() {
    let mut gate = MonitoringEventGate201::default();

    let record = recorded(&mut gate, call(0, false, 5), at(0));

    assert_eq!(record.events.len(), 5);
    assert_eq!(record.events_omitted, 0);
    assert_eq!((record.shed_calls, record.shed_events), (0, 0));
}

#[test]
fn the_record_cap_sheds_with_counts_until_the_window_rolls_over() {
    let mut gate = MonitoringEventGate201::new(limits(2, usize::MAX));
    recorded(&mut gate, call(0, true, 1), at(0));
    recorded(&mut gate, call(1, true, 1), at(1));

    assert_eq!(
        gate.admit(call(2, true, 4), at(2)),
        MonitoringEventDecision201::Shed {
            calls: 1,
            events: 4
        }
    );
    assert_eq!(
        gate.admit(call(3, true, 3), at(9)),
        MonitoringEventDecision201::Shed {
            calls: 2,
            events: 7
        }
    );

    let next = recorded(&mut gate, call(4, false, 1), at(10));
    assert_eq!((next.shed_calls, next.shed_events), (2, 7));
    assert_eq!(next.ordering, MonitoringEventOrdering201::InSequence);

    // The counts were reported once and are not repeated.
    let after = recorded(&mut gate, call(0, false, 1), at(11));
    assert_eq!((after.shed_calls, after.shed_events), (0, 0));
}

#[test]
fn a_new_connection_restarts_the_ordering_but_keeps_the_budget_and_the_shed_counts() {
    let mut gate = MonitoringEventGate201::new(limits(2, usize::MAX));
    recorded(&mut gate, call(0, true, 1), at(0));
    recorded(&mut gate, call(1, true, 1), at(1));
    assert_eq!(
        gate.admit(call(2, true, 4), at(2)),
        MonitoringEventDecision201::Shed {
            calls: 1,
            events: 4
        }
    );

    // The charger reconnects within the same window and starts a new series.
    gate.connection_started();
    assert_eq!(
        gate.admit(call(0, true, 3), at(3)),
        MonitoringEventDecision201::Shed {
            calls: 2,
            events: 7
        }
    );

    // The window rolls over: the first record reports every loss, and its ordering is judged
    // against the new connection's series, which the shed call at number 0 already advanced.
    let record = recorded(&mut gate, call(5, false, 1), at(10));
    assert_eq!((record.shed_calls, record.shed_events), (2, 7));
    assert_eq!(
        record.ordering,
        MonitoringEventOrdering201::Gap { expected: 1 }
    );
}

#[test]
fn a_new_connection_forgets_the_previous_connections_open_series() {
    let mut gate = MonitoringEventGate201::new(limits(10, usize::MAX));
    recorded(&mut gate, call(0, true, 1), at(0));

    gate.connection_started();

    assert_eq!(
        ordering(&mut gate, 0, true),
        MonitoringEventOrdering201::InSequence
    );
    gate.connection_started();
    assert_eq!(
        ordering(&mut gate, 1, false),
        MonitoringEventOrdering201::Gap { expected: 0 }
    );
}

#[test]
fn the_byte_cap_sheds_a_call_whose_record_would_exceed_the_window() {
    let one = {
        let mut probe = MonitoringEventGate201::default();
        encoded_len(&recorded(&mut probe, call(0, false, 3), at(0)))
    };
    let mut gate = MonitoringEventGate201::new(limits(100, one * 2 + one / 2));
    recorded(&mut gate, call(0, false, 3), at(0));
    recorded(&mut gate, call(0, false, 3), at(1));

    assert_eq!(
        gate.admit(call(0, false, 3), at(2)),
        MonitoringEventDecision201::Shed {
            calls: 1,
            events: 3
        }
    );
    // A smaller record still fits the remaining bytes and reports the shed call.
    let small = recorded(&mut gate, call(0, false, 1), at(3));
    assert_eq!((small.shed_calls, small.shed_events), (1, 3));
    // A rolled-over window admits the large one again.
    let again = recorded(&mut gate, call(0, false, 3), at(10));
    assert_eq!(again.events.len(), 3);
}

#[test]
fn shed_counts_survive_a_failed_commit_and_are_reported_by_the_retry() {
    let mut gate = MonitoringEventGate201::new(limits(1, usize::MAX));
    recorded(&mut gate, call(0, true, 1), at(0));
    gate.admit(call(1, true, 5), at(1));

    // The window rolled over, but the write fails: the receipt is dropped unconfirmed.
    let failed = gate.admit(call(2, false, 1), at(10));
    let MonitoringEventDecision201::Record { record, .. } = &failed else {
        panic!("expected a journal record");
    };
    assert_eq!((record.shed_calls, record.shed_events), (1, 5));
    drop(failed);

    // The next window admits the retry, which still carries the same counts.
    let retry = recorded(&mut gate, call(2, false, 1), at(20));
    assert_eq!((retry.shed_calls, retry.shed_events), (1, 5));
    let after = recorded(&mut gate, call(0, false, 1), at(30));
    assert_eq!((after.shed_calls, after.shed_events), (0, 0));
}

#[test]
fn shed_counters_saturate_instead_of_wrapping() {
    let mut gate = MonitoringEventGate201::new(limits(0, usize::MAX));
    gate.shed_calls = u32::MAX - 1;
    gate.shed_events = u32::MAX - 2;

    gate.admit(call(0, false, 5), at(0));
    let decision = gate.admit(call(0, false, 5), at(0));

    assert_eq!(
        decision,
        MonitoringEventDecision201::Shed {
            calls: u32::MAX,
            events: u32::MAX
        }
    );
}

#[test]
fn a_clock_moving_backwards_starts_a_fresh_window() {
    let mut gate = MonitoringEventGate201::new(limits(1, usize::MAX));
    recorded(&mut gate, call(0, false, 1), at(100));

    assert!(matches!(
        gate.admit(call(0, false, 1), at(101)),
        MonitoringEventDecision201::Shed { .. }
    ));
    recorded(&mut gate, call(0, false, 1), at(50));
}

#[test]
fn shedding_raises_the_shared_pressure_signal_once_per_call() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    assert_eq!(budget.snapshot().dropped_diagnostics, 0);

    record_monitoring_event_shed_201(&budget);
    record_monitoring_event_shed_201(&budget);

    assert_eq!(budget.snapshot().dropped_diagnostics, 2);
}
