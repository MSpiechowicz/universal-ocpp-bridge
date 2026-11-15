use crate::security_event16::{
    SecurityEventDecision16, SecurityEventGate16, SecurityEventLimits16,
};
use crate::{RuntimeResourceBudget, RuntimeResourceLimits};
use std::time::Duration;
use uob_contracts::UtcTimestamp;

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}

fn gate(records: u32) -> SecurityEventGate16 {
    SecurityEventGate16::new(SecurityEventLimits16 {
        window: Duration::from_secs(10),
        records,
    })
}

#[test]
fn the_default_budget_is_sixty_records_per_minute() {
    let mut gate = SecurityEventGate16::default();

    for _ in 0..60 {
        assert!(matches!(
            gate.admit(at(0)),
            SecurityEventDecision16::Store { shed_calls: 0, .. }
        ));
    }
    assert_eq!(
        gate.admit(at(59)),
        SecurityEventDecision16::Shed { calls: 1 }
    );
    assert!(matches!(
        gate.admit(at(60)),
        SecurityEventDecision16::Store { .. }
    ));
}

#[test]
fn calls_over_budget_are_shed_and_counted() {
    let mut gate = gate(2);

    for _ in 0..2 {
        assert!(matches!(
            gate.admit(at(0)),
            SecurityEventDecision16::Store { .. }
        ));
    }

    assert_eq!(
        gate.admit(at(1)),
        SecurityEventDecision16::Shed { calls: 1 }
    );
    assert_eq!(
        gate.admit(at(9)),
        SecurityEventDecision16::Shed { calls: 2 }
    );
}

#[test]
fn the_next_stored_record_reports_the_shed_calls_once_it_is_durable() {
    let mut gate = gate(1);
    let SecurityEventDecision16::Store { receipt, .. } = gate.admit(at(0)) else {
        panic!("first call is stored");
    };
    gate.committed(receipt);
    gate.admit(at(1));
    gate.admit(at(2));

    let SecurityEventDecision16::Store {
        shed_calls,
        receipt,
    } = gate.admit(at(10))
    else {
        panic!("the window rolled over");
    };
    assert_eq!(shed_calls, 2);
    gate.committed(receipt);

    let SecurityEventDecision16::Store { shed_calls, .. } = gate.admit(at(20)) else {
        panic!("a new window stores again");
    };
    assert_eq!(
        shed_calls, 0,
        "a committed record reports its loss only once"
    );
}

#[test]
fn an_uncommitted_record_keeps_the_shed_calls_for_the_retry() {
    let mut gate = gate(1);
    gate.admit(at(0));
    gate.admit(at(1));

    // The write behind this admission fails: the receipt is dropped.
    let SecurityEventDecision16::Store { shed_calls, .. } = gate.admit(at(10)) else {
        panic!("the window rolled over");
    };
    assert_eq!(shed_calls, 1);

    let SecurityEventDecision16::Store {
        shed_calls,
        receipt,
    } = gate.admit(at(20))
    else {
        panic!("the retry is stored");
    };
    assert_eq!(shed_calls, 1, "the failed attempt did not clear the count");
    gate.committed(receipt);
}

#[test]
fn shed_calls_arriving_after_a_stored_one_are_not_cleared_by_its_commit() {
    let mut gate = gate(2);
    let SecurityEventDecision16::Store { receipt, .. } = gate.admit(at(0)) else {
        panic!("stored");
    };
    gate.admit(at(0));
    // A third call is shed between the admission and the commit of the first.
    gate.admit(at(0));
    gate.committed(receipt);

    let SecurityEventDecision16::Store { shed_calls, .. } = gate.admit(at(10)) else {
        panic!("the window rolled over");
    };
    assert_eq!(shed_calls, 1);
}

#[test]
fn the_window_rolls_over_after_its_length_and_when_the_clock_moves_back() {
    let mut gate = gate(1);
    gate.admit(at(0));

    assert!(matches!(
        gate.admit(at(9)),
        SecurityEventDecision16::Shed { .. }
    ));
    assert!(matches!(
        gate.admit(at(10)),
        SecurityEventDecision16::Store { .. }
    ));
    assert!(matches!(
        gate.admit(at(11)),
        SecurityEventDecision16::Shed { .. }
    ));
    assert!(matches!(
        gate.admit(at(5)),
        SecurityEventDecision16::Store { .. }
    ));
}

#[test]
fn a_shed_call_raises_the_shared_resource_pressure_signal() {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    let before = budget.snapshot().dropped_diagnostics;

    crate::record_security_event_shed_16(&budget);

    assert_eq!(budget.snapshot().dropped_diagnostics, before + 1);
}
