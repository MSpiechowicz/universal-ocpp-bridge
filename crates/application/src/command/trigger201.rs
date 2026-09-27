use uob_contracts::{
    TriggerEvse201, TriggerMessageClass201, TriggerObservation201, TriggerObservationStatus201,
    TriggerTarget201, UtcTimestamp,
};

use crate::{CommandAdmissionError, CommandAdmissionErrorCode};

/// Native 2.0.1 scope and bounded targets captured from a trusted station snapshot before send.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriggerExpectation201 {
    pub requested_class: TriggerMessageClass201,
    pub native_scope: Option<TriggerEvse201>,
    pub expected_targets: Vec<TriggerTarget201>,
}

impl TriggerExpectation201 {
    /// Fixes the native evidence set and one-minute observation window at dispatch.
    ///
    /// # Errors
    /// Rejects incompatible, duplicate, empty, or excessive target sets and deadline overflow.
    pub fn start(&self, now: UtcTimestamp) -> Result<TriggerObservation201, CommandAdmissionError> {
        let targets = &self.expected_targets;
        if targets.is_empty()
            || targets.len() > 65
            || targets
                .iter()
                .enumerate()
                .any(|(index, target)| targets[..index].contains(target))
            || targets.iter().any(|target| match target {
                TriggerTarget201::Station => false,
                TriggerTarget201::Evse { id } => *id == 0,
                TriggerTarget201::Connector { id, connector_id } => *id == 0 || *connector_id == 0,
            })
            || self
                .native_scope
                .is_some_and(|scope| scope.id == 0 || scope.connector_id == Some(0))
        {
            return Err(invalid_expectation());
        }

        let valid = if self.requested_class.is_station_only() {
            targets.as_slice() == [TriggerTarget201::Station]
        } else if self.requested_class == TriggerMessageClass201::StatusNotification {
            match self.native_scope {
                Some(TriggerEvse201 {
                    id,
                    connector_id: Some(connector_id),
                }) => targets.as_slice() == [TriggerTarget201::Connector { id, connector_id }],
                _ => false,
            }
        } else if self.requested_class.is_certificate() {
            match self.native_scope {
                Some(scope) => targets.as_slice() == [TriggerTarget201::Evse { id: scope.id }],
                None => targets.as_slice() == [TriggerTarget201::Station],
            }
        } else {
            match (self.requested_class, self.native_scope) {
                (
                    TriggerMessageClass201::TransactionEvent,
                    Some(TriggerEvse201 {
                        id,
                        connector_id: Some(connector_id),
                    }),
                ) => targets.as_slice() == [TriggerTarget201::Connector { id, connector_id }],
                (_, scope) => {
                    let no_connectors = targets
                        .iter()
                        .all(|target| !matches!(target, TriggerTarget201::Connector { .. }));
                    no_connectors
                        && match scope {
                            Some(scope) => {
                                targets.as_slice() == [TriggerTarget201::Evse { id: scope.id }]
                            }
                            None => true,
                        }
                }
            }
        };
        if !valid {
            return Err(invalid_expectation());
        }

        let deadline = now
            .into_inner()
            .checked_add(time::Duration::seconds(60))
            .ok_or_else(invalid_expectation)?;
        Ok(TriggerObservation201 {
            requested_class: self.requested_class,
            native_scope: self.native_scope,
            expected_targets: targets.clone(),
            dispatch_started_at: now,
            deadline: UtcTimestamp::new(deadline),
            native_response: None,
            observed: Vec::new(),
            status: TriggerObservationStatus201::Pending,
        })
    }
}

fn invalid_expectation() -> CommandAdmissionError {
    CommandAdmissionError::new(
        CommandAdmissionErrorCode::InvalidRequest,
        "invalid OCPP 2.0.1 trigger expectation",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_scope_preserves_connector_but_evidence_is_evse_only() {
        let now: UtcTimestamp = serde_json::from_str("\"2026-09-01T00:00:00Z\"").unwrap();
        let scope = TriggerEvse201 {
            id: 4,
            connector_id: Some(2),
        };
        let expectation = TriggerExpectation201 {
            requested_class: TriggerMessageClass201::MeterValues,
            native_scope: Some(scope),
            expected_targets: vec![TriggerTarget201::Evse { id: 4 }],
        };
        let observation = expectation.start(now).unwrap();
        assert_eq!(observation.native_scope, Some(scope));
        assert_eq!(
            observation.expected_targets,
            vec![TriggerTarget201::Evse { id: 4 }]
        );
        let status = TriggerExpectation201 {
            requested_class: TriggerMessageClass201::StatusNotification,
            ..expectation
        };
        assert!(status.start(now).is_err());
    }

    #[test]
    fn transaction_event_connector_scope_requires_matching_connector_evidence() {
        let now: UtcTimestamp = serde_json::from_str("\"2026-09-01T00:00:00Z\"").unwrap();
        let expectation = TriggerExpectation201 {
            requested_class: TriggerMessageClass201::TransactionEvent,
            native_scope: Some(TriggerEvse201 {
                id: 4,
                connector_id: Some(2),
            }),
            expected_targets: vec![TriggerTarget201::Connector {
                id: 4,
                connector_id: 2,
            }],
        };

        let observation = expectation.start(now).unwrap();
        assert_eq!(
            observation.expected_targets,
            vec![TriggerTarget201::Connector {
                id: 4,
                connector_id: 2,
            }]
        );

        let wrong_target = TriggerExpectation201 {
            expected_targets: vec![TriggerTarget201::Evse { id: 4 }],
            ..expectation
        };
        assert!(wrong_target.start(now).is_err());
    }
}
