use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{EventId, UtcTimestamp};

/// The eleven pinned OCPP 2.0.1 `TriggerMessage` `requestedMessage` values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum TriggerMessageClass201 {
    BootNotification,
    LogStatusNotification,
    FirmwareStatusNotification,
    Heartbeat,
    MeterValues,
    SignChargingStationCertificate,
    SignV2GCertificate,
    StatusNotification,
    TransactionEvent,
    SignCombinedCertificate,
    PublishFirmwareStatusNotification,
}

impl TriggerMessageClass201 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BootNotification => "BootNotification",
            Self::LogStatusNotification => "LogStatusNotification",
            Self::FirmwareStatusNotification => "FirmwareStatusNotification",
            Self::Heartbeat => "Heartbeat",
            Self::MeterValues => "MeterValues",
            Self::SignChargingStationCertificate => "SignChargingStationCertificate",
            Self::SignV2GCertificate => "SignV2GCertificate",
            Self::StatusNotification => "StatusNotification",
            Self::TransactionEvent => "TransactionEvent",
            Self::SignCombinedCertificate => "SignCombinedCertificate",
            Self::PublishFirmwareStatusNotification => "PublishFirmwareStatusNotification",
        }
    }

    #[must_use]
    pub const fn is_certificate(self) -> bool {
        matches!(
            self,
            Self::SignChargingStationCertificate
                | Self::SignV2GCertificate
                | Self::SignCombinedCertificate
        )
    }

    #[must_use]
    pub const fn is_station_only(self) -> bool {
        matches!(
            self,
            Self::BootNotification
                | Self::LogStatusNotification
                | Self::FirmwareStatusNotification
                | Self::Heartbeat
                | Self::SignChargingStationCertificate
                | Self::PublishFirmwareStatusNotification
        )
    }
}

/// Native EVSE identity, retaining the independent connector index within that EVSE.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerEvse201 {
    pub id: u32,
    #[serde(
        rename = "connectorId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub connector_id: Option<u32>,
}

/// Native target evidence. A connector is never conflated with its EVSE ID.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerTarget201 {
    Station,
    Evse { id: u32 },
    Connector { id: u32, connector_id: u32 },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum TriggerNativeStatus201 {
    Accepted,
    Rejected,
    NotImplemented,
}

/// Native statusInfo is response detail, not evidence of a later notification.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerStatusInfo201 {
    #[serde(rename = "reasonCode")]
    pub reason_code: String,
    #[serde(
        rename = "additionalInfo",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_info: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerNativeResponse201 {
    pub status: TriggerNativeStatus201,
    #[serde(
        rename = "statusInfo",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub status_info: Option<TriggerStatusInfo201>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TriggerObservedEvent201 {
    pub event_id: EventId,
    pub target: TriggerTarget201,
    pub observed_at: UtcTimestamp,
}

/// Unattributable means station-level certificate receipt cannot prove the scoped EVSE.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerObservationStatus201 {
    Pending,
    Partial,
    Observed,
    Absent,
    Unsupported,
    Unattributable,
}

/// Immutable targets/window and bounded durable later compatible-message observations.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TriggerObservation201 {
    pub requested_class: TriggerMessageClass201,
    pub native_scope: Option<TriggerEvse201>,
    pub expected_targets: Vec<TriggerTarget201>,
    pub dispatch_started_at: UtcTimestamp,
    pub deadline: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_response: Option<TriggerNativeResponse201>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed: Vec<TriggerObservedEvent201>,
    pub status: TriggerObservationStatus201,
}

impl TriggerObservation201 {
    /// Recomputes independent post-response evidence; receipt alone is never protocol acceptance.
    pub fn refresh_status(&mut self, now: UtcTimestamp) {
        if self
            .native_response
            .as_ref()
            .is_some_and(|response| response.status != TriggerNativeStatus201::Accepted)
        {
            self.status = TriggerObservationStatus201::Unsupported;
            return;
        }

        let scoped_certificate = self.requested_class.is_certificate()
            && !self.requested_class.is_station_only()
            && self.native_scope.is_some();
        let seen = self
            .expected_targets
            .iter()
            .enumerate()
            .filter(|(index, target)| {
                self.observed.iter().any(|event| {
                    !scoped_certificate
                        && self.compatible_target(**target, event.target)
                        && !self.expected_targets[..*index].iter().any(|prior| {
                            self.observed.iter().any(|other| {
                                self.compatible_target(*prior, other.target)
                                    && other.event_id == event.event_id
                            })
                        })
                })
            })
            .count();
        let station_certificate = scoped_certificate
            && self
                .observed
                .iter()
                .any(|event| event.target == TriggerTarget201::Station);

        self.status = if self
            .native_response
            .as_ref()
            .is_some_and(|response| response.status == TriggerNativeStatus201::Accepted)
            && seen == self.expected_targets.len()
            && seen != 0
            && !station_certificate
        {
            TriggerObservationStatus201::Observed
        } else if station_certificate {
            TriggerObservationStatus201::Unattributable
        } else if now >= self.deadline {
            if seen == 0 {
                TriggerObservationStatus201::Absent
            } else {
                TriggerObservationStatus201::Partial
            }
        } else if seen != 0
            && self
                .native_response
                .as_ref()
                .is_some_and(|response| response.status == TriggerNativeStatus201::Accepted)
        {
            TriggerObservationStatus201::Partial
        } else {
            TriggerObservationStatus201::Pending
        };
    }

    fn compatible_target(&self, expected: TriggerTarget201, observed: TriggerTarget201) -> bool {
        expected == observed
            || (self.requested_class == TriggerMessageClass201::TransactionEvent
                && matches!(
                    (expected, observed),
                    (TriggerTarget201::Evse { id: expected }, TriggerTarget201::Connector { id: actual, .. })
                        if expected == actual
                ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(value: &str) -> UtcTimestamp {
        serde_json::from_str(&format!("\"{value}\"")).unwrap()
    }

    #[test]
    fn accepted_reply_needs_distinct_evse_evidence_for_each_frozen_target() {
        let start = timestamp("2026-09-01T00:00:00Z");
        let later = timestamp("2026-09-01T00:00:04Z");
        let mut observation = TriggerObservation201 {
            requested_class: TriggerMessageClass201::MeterValues,
            native_scope: None,
            expected_targets: vec![
                TriggerTarget201::Evse { id: 1 },
                TriggerTarget201::Evse { id: 2 },
            ],
            dispatch_started_at: start,
            deadline: timestamp("2026-09-01T00:01:00Z"),
            native_response: Some(TriggerNativeResponse201 {
                status: TriggerNativeStatus201::Accepted,
                status_info: Some(TriggerStatusInfo201 {
                    reason_code: "Ready".to_owned(),
                    additional_info: None,
                }),
            }),
            observed: vec![],
            status: TriggerObservationStatus201::Pending,
        };
        observation.refresh_status(later);
        assert_eq!(observation.status, TriggerObservationStatus201::Pending);
        let id = EventId::new("meter-1").unwrap();
        observation.observed.push(TriggerObservedEvent201 {
            event_id: id.clone(),
            target: TriggerTarget201::Evse { id: 1 },
            observed_at: later,
        });
        observation.observed.push(TriggerObservedEvent201 {
            event_id: id,
            target: TriggerTarget201::Evse { id: 2 },
            observed_at: later,
        });
        observation.refresh_status(later);
        assert_eq!(observation.status, TriggerObservationStatus201::Partial);
        observation.observed[1].event_id = EventId::new("meter-2").unwrap();
        observation.refresh_status(later);
        assert_eq!(observation.status, TriggerObservationStatus201::Observed);
    }

    #[test]
    fn station_certificate_receipt_cannot_prove_scoped_evse() {
        let start = timestamp("2026-09-01T00:00:00Z");
        let mut observation = TriggerObservation201 {
            requested_class: TriggerMessageClass201::SignV2GCertificate,
            native_scope: Some(TriggerEvse201 {
                id: 2,
                connector_id: None,
            }),
            expected_targets: vec![TriggerTarget201::Evse { id: 2 }],
            dispatch_started_at: start,
            deadline: timestamp("2026-09-01T00:01:00Z"),
            native_response: Some(TriggerNativeResponse201 {
                status: TriggerNativeStatus201::Accepted,
                status_info: None,
            }),
            observed: vec![TriggerObservedEvent201 {
                event_id: EventId::new("certificate-1").unwrap(),
                target: TriggerTarget201::Station,
                observed_at: start,
            }],
            status: TriggerObservationStatus201::Pending,
        };
        observation.refresh_status(timestamp("2026-09-01T00:02:00Z"));
        assert_eq!(
            observation.status,
            TriggerObservationStatus201::Unattributable
        );
        assert_eq!(observation.observed[0].target, TriggerTarget201::Station);
    }

    #[test]
    fn transaction_evse_accepts_its_connector_but_exact_connector_rejects_its_sibling() {
        let start = timestamp("2026-09-01T00:00:00Z");
        let mut observation = TriggerObservation201 {
            requested_class: TriggerMessageClass201::TransactionEvent,
            native_scope: Some(TriggerEvse201 {
                id: 3,
                connector_id: None,
            }),
            expected_targets: vec![TriggerTarget201::Evse { id: 3 }],
            dispatch_started_at: start,
            deadline: timestamp("2026-09-01T00:01:00Z"),
            native_response: Some(TriggerNativeResponse201 {
                status: TriggerNativeStatus201::Accepted,
                status_info: None,
            }),
            observed: vec![TriggerObservedEvent201 {
                event_id: EventId::new("tx-1").unwrap(),
                target: TriggerTarget201::Connector {
                    id: 3,
                    connector_id: 2,
                },
                observed_at: start,
            }],
            status: TriggerObservationStatus201::Pending,
        };
        observation.refresh_status(start);
        assert_eq!(observation.status, TriggerObservationStatus201::Observed);
        observation.native_scope = Some(TriggerEvse201 {
            id: 3,
            connector_id: Some(1),
        });
        observation.expected_targets = vec![TriggerTarget201::Connector {
            id: 3,
            connector_id: 1,
        }];
        observation.refresh_status(start);
        assert_eq!(observation.status, TriggerObservationStatus201::Pending);
    }

    #[test]
    fn station_certificate_receipt_satisfies_station_trigger_even_with_ignored_evse_scope() {
        let start = timestamp("2026-09-01T00:00:00Z");
        let mut observation = TriggerObservation201 {
            requested_class: TriggerMessageClass201::SignChargingStationCertificate,
            native_scope: Some(TriggerEvse201 {
                id: 2,
                connector_id: Some(1),
            }),
            expected_targets: vec![TriggerTarget201::Station],
            dispatch_started_at: start,
            deadline: timestamp("2026-09-01T00:01:00Z"),
            native_response: Some(TriggerNativeResponse201 {
                status: TriggerNativeStatus201::Accepted,
                status_info: None,
            }),
            observed: vec![TriggerObservedEvent201 {
                event_id: EventId::new("station-certificate").unwrap(),
                target: TriggerTarget201::Station,
                observed_at: start,
            }],
            status: TriggerObservationStatus201::Pending,
        };
        observation.refresh_status(start);
        assert_eq!(observation.status, TriggerObservationStatus201::Observed);
    }
}
