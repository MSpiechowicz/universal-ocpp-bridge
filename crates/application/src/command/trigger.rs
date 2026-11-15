use uob_contracts::{
    CommandResult, ContractVersion, TriggerMessageClass, TriggerObservation,
    TriggerObservationStatus, UtcTimestamp,
};

use super::TriggerExpectation201;
use crate::{CommandAdmissionError, CommandAdmissionErrorCode};

/// Edition-discriminated expectation; dispatch never erases the native resource model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TriggerExpectation {
    Ocpp16(TriggerExpectation16),
    Ocpp201(TriggerExpectation201),
}

impl TriggerExpectation {
    pub(super) fn start(
        &self,
        result: &mut CommandResult,
        now: UtcTimestamp,
    ) -> Result<(), CommandAdmissionError> {
        match self {
            Self::Ocpp16(expectation) => {
                let observation = expectation.start(now)?;
                result.schema_version = observation.requested_class.contract_version();
                result.trigger_observation = Some(observation);
            }
            Self::Ocpp201(expectation) => {
                result.schema_version = ContractVersion::V1_TRIGGER_201;
                result.trigger_observation_201 = Some(expectation.start(now)?);
            }
        }
        Ok(())
    }
}

impl TriggerExpectation {
    pub(super) fn finish(
        &self,
        result: &mut CommandResult,
        dispatch_started_at: UtcTimestamp,
        now: UtcTimestamp,
        response_16: Option<uob_contracts::TriggerNativeResponse>,
        response_201: Option<uob_contracts::TriggerNativeResponse201>,
    ) -> Result<(), CommandAdmissionError> {
        self.start(result, dispatch_started_at)?;
        match self {
            Self::Ocpp16(_) => {
                let observation = result
                    .trigger_observation
                    .as_mut()
                    .expect("1.6 expectation");
                observation.native_response = response_16;
                observation.refresh_status(now);
            }
            Self::Ocpp201(_) => {
                let observation = result
                    .trigger_observation_201
                    .as_mut()
                    .expect("201 expectation");
                observation.native_response = response_201;
                observation.refresh_status(now);
            }
        }
        Ok(())
    }
}

/// Native OCPP 1.6 target set captured before a `TriggerMessage` is sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriggerExpectation16 {
    pub requested_class: TriggerMessageClass,
    /// `None` requests all applicable IDs; explicit zero means station-only for `StatusNotification`.
    pub native_scope: Option<u32>,
    /// Immutable native targets. Zero represents the station, not a wildcard.
    pub expected_targets: Vec<u32>,
}

impl TriggerExpectation16 {
    /// Fixes the observation window at the actual dispatch start.
    ///
    /// # Errors
    /// Returns an invalid request for empty, duplicate or incompatible targets, or a deadline overflow.
    pub fn start(&self, now: UtcTimestamp) -> Result<TriggerObservation, CommandAdmissionError> {
        let targets = &self.expected_targets;
        if targets.is_empty()
            || targets.len() > 65
            || targets
                .iter()
                .enumerate()
                .any(|(index, target)| targets[..index].contains(target))
        {
            return Err(invalid_expectation());
        }
        let station_only = self.requested_class.is_station_only();
        if (station_only && targets.as_slice() != [0])
            || (self.requested_class == TriggerMessageClass::MeterValues
                && (targets.contains(&0) || targets.len() > 64))
            || (self.requested_class == TriggerMessageClass::StatusNotification
                && self.native_scope == Some(0)
                && targets.as_slice() != [0])
            || (self.requested_class == TriggerMessageClass::StatusNotification
                && self.native_scope.is_none()
                && !targets.contains(&0))
            || self
                .native_scope
                .is_some_and(|id| id > 0 && targets.as_slice() != [id] && !station_only)
        {
            return Err(invalid_expectation());
        }
        let deadline = now
            .into_inner()
            .checked_add(time::Duration::seconds(60))
            .ok_or_else(invalid_expectation)?;
        Ok(TriggerObservation {
            requested_class: self.requested_class,
            native_scope: self.native_scope,
            expected_targets: self.expected_targets.clone(),
            dispatch_started_at: now,
            deadline: UtcTimestamp::new(deadline),
            native_response: None,
            observed: Vec::new(),
            status: TriggerObservationStatus::Pending,
        })
    }
}

fn invalid_expectation() -> CommandAdmissionError {
    CommandAdmissionError::new(
        CommandAdmissionErrorCode::InvalidRequest,
        "invalid OCPP 1.6 trigger expectation",
    )
}

#[cfg(test)]
mod tests {
    use uob_contracts::{
        AuthenticatedCommandOrigin, BridgeId, Command, CommandLifecycle, CommandOperation,
        PayloadSchemaId, PrincipalId, PrivilegedOcppOperation, ProtocolActionName, ProtocolEdition,
        RequestId, ResourceRef, StationId,
    };

    use super::*;

    fn blank_result(now: UtcTimestamp) -> CommandResult {
        let command = Command {
            schema_version: ContractVersion::V1_INITIAL,
            request_id: RequestId::new("trigger-revision").unwrap(),
            correlation_id: None,
            resource: ResourceRef {
                bridge_id: BridgeId::new("bridge").unwrap(),
                station_id: StationId::new("charger").unwrap(),
                resource: None,
                native_protocol_reference: None,
            },
            operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
                protocol: ProtocolEdition::Ocpp16j,
                action: ProtocolActionName::new("ExtendedTriggerMessage").unwrap(),
                payload_schema: PayloadSchemaId::new("extended-trigger").unwrap(),
                payload: (),
            }),
            expires_at: now,
            origin: AuthenticatedCommandOrigin::Management {
                principal_id: PrincipalId::new("operator").unwrap(),
            },
            admitted_at: now,
        };
        crate::command::results::command_result(&command, CommandLifecycle::Dispatched, now)
    }

    #[test]
    fn all_status_targets_include_zero_and_sixty_four_connectors() {
        let now: UtcTimestamp = serde_json::from_str("\"2026-09-01T00:00:00Z\"").unwrap();
        let targets = (0..=64).collect::<Vec<_>>();
        let status = TriggerExpectation16 {
            requested_class: TriggerMessageClass::StatusNotification,
            native_scope: None,
            expected_targets: targets.clone(),
        };
        let observation = status.start(now).expect("station and connectors fit");
        assert_eq!(observation.expected_targets, targets);
        assert_eq!(
            observation.deadline.into_inner() - observation.dispatch_started_at.into_inner(),
            time::Duration::seconds(60)
        );
        let meter = TriggerExpectation16 {
            requested_class: TriggerMessageClass::MeterValues,
            ..status
        };
        assert!(meter.start(now).is_err());
        let meter = TriggerExpectation16 {
            expected_targets: (1..=64).collect(),
            ..meter
        };
        assert!(meter.start(now).is_ok());
    }
    #[test]
    fn station_only_classes_ignore_irrelevant_native_connector() {
        let now: UtcTimestamp = serde_json::from_str("\"2026-09-01T00:00:00Z\"").unwrap();
        for requested_class in [
            TriggerMessageClass::BootNotification,
            TriggerMessageClass::DiagnosticsStatusNotification,
            TriggerMessageClass::FirmwareStatusNotification,
            TriggerMessageClass::Heartbeat,
            TriggerMessageClass::LogStatusNotification,
            TriggerMessageClass::SignedFirmwareStatusNotification,
        ] {
            let observation = TriggerExpectation16 {
                requested_class,
                native_scope: Some(42),
                expected_targets: vec![0],
            }
            .start(now)
            .expect("irrelevant connector ignored");
            assert_eq!(observation.native_scope, Some(42));
            assert_eq!(observation.expected_targets, vec![0]);
        }
    }

    #[test]
    fn whitepaper_classes_are_station_only_and_carry_revision_19() {
        let now: UtcTimestamp = serde_json::from_str("\"2026-09-01T00:00:00Z\"").unwrap();
        for requested_class in [
            TriggerMessageClass::LogStatusNotification,
            TriggerMessageClass::SignedFirmwareStatusNotification,
        ] {
            let station = TriggerExpectation16 {
                requested_class,
                native_scope: None,
                expected_targets: vec![0],
            };
            assert!(station.start(now).is_ok());
            let connector = TriggerExpectation16 {
                expected_targets: vec![1],
                ..station.clone()
            };
            assert!(connector.start(now).is_err());
            let both = TriggerExpectation16 {
                expected_targets: vec![0, 1],
                ..station
            };
            assert!(both.start(now).is_err());
            assert_eq!(
                requested_class.contract_version(),
                ContractVersion::V1_EXTENDED_TRIGGER_16
            );
            assert_eq!(ContractVersion::V1_EXTENDED_TRIGGER_16.revision, 19);
        }
    }

    #[test]
    fn expectation_start_sets_result_revision_from_class() {
        let now: UtcTimestamp = serde_json::from_str("\"2026-09-01T00:00:00Z\"").unwrap();
        for (requested_class, revision) in [
            (TriggerMessageClass::LogStatusNotification, 19),
            (TriggerMessageClass::SignedFirmwareStatusNotification, 19),
            (TriggerMessageClass::BootNotification, 2),
            (TriggerMessageClass::FirmwareStatusNotification, 2),
            (TriggerMessageClass::Heartbeat, 2),
        ] {
            let expectation = TriggerExpectation::Ocpp16(TriggerExpectation16 {
                requested_class,
                native_scope: None,
                expected_targets: vec![0],
            });
            let mut result = blank_result(now);
            expectation.start(&mut result, now).unwrap();
            assert_eq!(
                result.schema_version.revision, revision,
                "{requested_class:?}"
            );
        }
    }
}
