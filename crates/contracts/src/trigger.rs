use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{EventId, UtcTimestamp};

/// OCPP 1.6 message requested by `TriggerMessage`; never an extended or 2.0.1 action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum TriggerMessageClass {
    BootNotification,
    DiagnosticsStatusNotification,
    FirmwareStatusNotification,
    Heartbeat,
    MeterValues,
    StatusNotification,
}
impl TriggerMessageClass {
    /// Native OCPP 1.6 action name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BootNotification => "BootNotification",
            Self::DiagnosticsStatusNotification => "DiagnosticsStatusNotification",
            Self::FirmwareStatusNotification => "FirmwareStatusNotification",
            Self::Heartbeat => "Heartbeat",
            Self::MeterValues => "MeterValues",
            Self::StatusNotification => "StatusNotification",
        }
    }
}

/// Exact native response, independent of later station messages.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum TriggerNativeResponse {
    Accepted,
    Rejected,
    NotImplemented,
}

/// Evidence from one durable, compatible station-origin message, not causal proof.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TriggerObservedEvent {
    pub event_id: EventId,
    /// Native connector number; zero identifies the station, not all connectors.
    pub target: u32,
    pub observed_at: UtcTimestamp,
}

/// Later compatible-message observation, distinct from protocol acceptance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerObservationStatus {
    Pending,
    Partial,
    Observed,
    Absent,
    Unsupported,
}

/// Immutable dispatch expectation and bounded later observations for `TriggerMessage`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TriggerObservation {
    pub requested_class: TriggerMessageClass,
    /// Original optional connectorId: None requests all applicable native IDs.
    pub native_scope: Option<u32>,
    /// Unique native connector IDs fixed before dispatch; zero is station-only.
    pub expected_targets: Vec<u32>,
    pub dispatch_started_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    /// A missing response is not Accepted, including after restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_response: Option<TriggerNativeResponse>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed: Vec<TriggerObservedEvent>,
    pub status: TriggerObservationStatus,
}

impl TriggerObservation {
    /// Derives the observation state only from the native reply, compatible events and deadline.
    pub fn refresh_status(&mut self, now: UtcTimestamp) {
        self.status = match self.native_response {
            Some(TriggerNativeResponse::Rejected | TriggerNativeResponse::NotImplemented) => {
                TriggerObservationStatus::Unsupported
            }
            None | Some(TriggerNativeResponse::Accepted) => {
                let seen = self
                    .expected_targets
                    .iter()
                    .enumerate()
                    .filter(|(index, target)| {
                        self.observed
                            .iter()
                            .find(|event| event.target == **target)
                            .is_some_and(|event| {
                                !self.expected_targets[..*index].iter().any(|previous| {
                                    self.observed.iter().any(|prior| {
                                        prior.target == *previous
                                            && prior.event_id == event.event_id
                                    })
                                })
                            })
                    })
                    .count();
                if self.native_response == Some(TriggerNativeResponse::Accepted)
                    && seen == self.expected_targets.len()
                    && seen != 0
                {
                    TriggerObservationStatus::Observed
                } else if now >= self.deadline {
                    if seen == 0 {
                        TriggerObservationStatus::Absent
                    } else {
                        TriggerObservationStatus::Partial
                    }
                } else if seen != 0 && self.native_response == Some(TriggerNativeResponse::Accepted)
                {
                    TriggerObservationStatus::Partial
                } else {
                    TriggerObservationStatus::Pending
                }
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_event_identity_never_completes_two_distinct_targets() {
        let start: UtcTimestamp = serde_json::from_str("\"2026-09-01T00:00:00Z\"").unwrap();
        let deadline: UtcTimestamp = serde_json::from_str("\"2026-09-01T00:01:00Z\"").unwrap();
        let event_id = EventId::new("same-event").unwrap();
        let mut observation = TriggerObservation {
            requested_class: TriggerMessageClass::StatusNotification,
            native_scope: None,
            expected_targets: vec![1, 2],
            dispatch_started_at: start,
            deadline,
            native_response: Some(TriggerNativeResponse::Accepted),
            observed: vec![
                TriggerObservedEvent {
                    event_id: event_id.clone(),
                    target: 1,
                    observed_at: start,
                },
                TriggerObservedEvent {
                    event_id,
                    target: 2,
                    observed_at: start,
                },
            ],
            status: TriggerObservationStatus::Pending,
        };
        observation.refresh_status(start);
        assert_eq!(observation.status, TriggerObservationStatus::Partial);
        observation.observed[1].event_id = EventId::new("second-event").unwrap();
        observation.refresh_status(start);
        assert_eq!(observation.status, TriggerObservationStatus::Observed);
    }
}
