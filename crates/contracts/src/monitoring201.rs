//! OCPP 2.0.1 variable monitoring (N02, N03, N07) and charger-initiated `NotifyEvent` evidence.
//! Monitors and reports are native facts about the charger's own device model; they are never
//! bridge policy. `customData` and free station text (`additionalInfo`) are not retained.
use crate::{
    DeviceComponent201, DeviceReportAck201, DeviceReportFailure201, DeviceReportProgress201,
    DeviceSelector201, DeviceVariable201, ExactDecimal, UtcTimestamp,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::fmt;

mod event;
pub use event::*;

/// Maximum escaped JSON bytes retained for a native monitoring command result.
pub const MONITORING_REPORT_OUTPUT_LIMIT_201: usize = 1024 * 1024;
/// Bound for one journaled `NotifyEvent` record. Events that would exceed it are omitted and
/// counted, so the record always fits the integration event stream's payload limit.
pub const MONITORING_EVENT_EVIDENCE_LIMIT_201: usize = 64 * 1024;
/// Maximum events retained in one journaled `NotifyEvent` record.
pub const MONITORING_EVENTS_PER_RECORD_LIMIT_201: usize = 32;

/// Native `MonitorEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum MonitorType201 {
    UpperThreshold,
    LowerThreshold,
    Delta,
    Periodic,
    PeriodicClockAligned,
}

/// Native `MonitoringCriterionEnumType`; criteria are OR-combined by the charger.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum MonitoringCriterion201 {
    ThresholdMonitoring,
    DeltaMonitoring,
    PeriodicMonitoring,
}

/// Native `MonitoringBaseEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum MonitoringBase201 {
    All,
    FactoryDefault,
    HardWiredOnly,
}

/// Native `SetMonitoringStatusEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum SetMonitoringStatus201 {
    Accepted,
    UnknownComponent,
    UnknownVariable,
    UnsupportedMonitorType,
    Rejected,
    Duplicate,
}

impl SetMonitoringStatus201 {
    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }
}

/// Native `ClearMonitoringStatusEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ClearMonitoringStatus201 {
    Accepted,
    Rejected,
    NotFound,
}

impl ClearMonitoringStatus201 {
    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }
}

/// Native `GenericStatusEnumType` answered to `SetMonitoringLevel`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum MonitoringLevelStatus201 {
    Accepted,
    Rejected,
}

/// Native `EventTriggerEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum EventTrigger201 {
    Alerting,
    Delta,
    Periodic,
}

/// Native `EventNotificationEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum EventNotification201 {
    HardWiredNotification,
    HardWiredMonitor,
    PreconfiguredMonitor,
    CustomMonitor,
}

/// Native severity, 0 (highest) to 9 (lowest): 0 Danger, 1 Hardware Failure, 2 System Failure,
/// 3 Critical, 4 Error, 5 Alert, 6 Warning, 7 Notice, 8 Informational, 9 Debug. Deserialization
/// rejects every other number.
#[derive(Clone, Copy, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MonitorSeverity201(#[schemars(range(min = 0, max = 9))] u8);

impl MonitorSeverity201 {
    /// Highest native severity number (Debug).
    pub const MAX: u8 = 9;

    /// Creates a severity after rejecting numbers above [`Self::MAX`].
    ///
    /// # Errors
    ///
    /// Returns [`MonitorSeverityError201`] when `value` is greater than 9.
    pub const fn new(value: u8) -> Result<Self, MonitorSeverityError201> {
        if value > Self::MAX {
            return Err(MonitorSeverityError201);
        }
        Ok(Self(value))
    }

    /// Native severity number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl<'de> Deserialize<'de> for MonitorSeverity201 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// A severity outside the native 0 to 9 range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MonitorSeverityError201;

impl fmt::Display for MonitorSeverityError201 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("monitor severity must be between 0 and 9")
    }
}

impl std::error::Error for MonitorSeverityError201 {}

/// Native `statusInfo.reasonCode`: 1 to 20 printable ASCII characters.
#[must_use]
pub fn valid_monitoring_reason_code_201(value: &str) -> bool {
    (1..=20).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn reason_code<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    if value
        .as_deref()
        .is_some_and(|code| !valid_monitoring_reason_code_201(code))
    {
        return Err(de::Error::custom(
            "reason code must be 1 to 20 printable ASCII characters",
        ));
    }
    Ok(value)
}

/// One native `SetMonitoringDataType`. `value` is the threshold or delta, or the interval in
/// seconds for periodic monitors; it is exact, never binary floating point.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetMonitor201 {
    /// Only given to replace an existing monitor; the charger assigns IDs to new ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i32>,
    /// Active only during a transaction on the component. Absent means native default false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<bool>,
    pub value: ExactDecimal,
    pub monitor_type: MonitorType201,
    pub severity: MonitorSeverity201,
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
}

/// Immutable request echo, one variant per native action.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum MonitoringQuery201 {
    SetVariableMonitoring {
        monitors: Vec<SetMonitor201>,
    },
    ClearVariableMonitoring {
        ids: Vec<i32>,
    },
    GetMonitoringReport {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        request_id: i32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        criteria: Vec<MonitoringCriterion201>,
        /// Native selectors; an absent variable keeps the component wildcard.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        component_variables: Vec<DeviceSelector201>,
    },
    SetMonitoringBase {
        base: MonitoringBase201,
    },
    SetMonitoringLevel {
        severity: MonitorSeverity201,
    },
}

impl MonitoringQuery201 {
    /// Native report correlation identity, only for `GetMonitoringReport`.
    #[must_use]
    pub const fn request_id(&self) -> Option<i32> {
        match self {
            Self::GetMonitoringReport { request_id, .. } => Some(*request_id),
            Self::SetVariableMonitoring { .. }
            | Self::ClearVariableMonitoring { .. }
            | Self::SetMonitoringBase { .. }
            | Self::SetMonitoringLevel { .. } => None,
        }
    }
}

/// Native `SetMonitoringResultType`; the ID is present only when the charger accepted.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetMonitorResult201 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i32>,
    pub status: SetMonitoringStatus201,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "reason_code"
    )]
    #[schemars(length(min = 1, max = 20), regex(pattern = "^[!-~]{1,20}$"))]
    pub reason_code: Option<String>,
    pub monitor_type: MonitorType201,
    pub severity: MonitorSeverity201,
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
}

/// Native `ClearMonitoringResultType`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClearMonitorResult201 {
    pub id: i32,
    pub status: ClearMonitoringStatus201,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "reason_code"
    )]
    #[schemars(length(min = 1, max = 20), regex(pattern = "^[!-~]{1,20}$"))]
    pub reason_code: Option<String>,
}

/// One monitor installed on the charger, as reported by `NotifyMonitoringReport`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedMonitor201 {
    pub component: DeviceComponent201,
    pub variable: DeviceVariable201,
    pub id: i32,
    pub transaction: bool,
    pub value: ExactDecimal,
    pub monitor_type: MonitorType201,
    pub severity: MonitorSeverity201,
}

/// Metadata for every accepted native report fragment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonitoringReportFragment201 {
    pub generated_at: UtcTimestamp,
    pub sequence: u32,
    pub more: bool,
    /// Monitors carried by this fragment, including any omitted from the result.
    pub monitors: u32,
}

/// Collection result, independent of the native CALL response.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MonitoringReportState201 {
    /// The action produces no report, or the charger did not accept one.
    NotExpected,
    /// Accepted; fragments are still being collected.
    Pending,
    Complete {
        fragments: Vec<MonitoringReportFragment201>,
        monitors: Vec<ReportedMonitor201>,
        /// Monitors received but not retained because of the output limit.
        #[serde(default, skip_serializing_if = "is_zero")]
        monitors_omitted: u32,
    },
    Incomplete {
        reason: DeviceReportFailure201,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<DeviceReportProgress201>,
    },
}

impl MonitoringReportState201 {
    #[must_use]
    pub const fn pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Native reply of the charger, one shape per action.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MonitoringOutcome201 {
    /// `SetVariableMonitoringResponse`, one independent result per requested monitor.
    SetMonitors { monitors: Vec<SetMonitorResult201> },
    /// `ClearVariableMonitoringResponse`, one independent result per requested ID.
    ClearMonitors {
        monitors: Vec<ClearMonitorResult201>,
    },
    /// `SetMonitoringBaseResponse`.
    Base { status: DeviceReportAck201 },
    /// `SetMonitoringLevelResponse`.
    Level { status: MonitoringLevelStatus201 },
    /// `GetMonitoringReportResponse` and the separately collected report.
    Report {
        /// Absent when no valid CALLRESULT was received.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ack: Option<DeviceReportAck201>,
        report: MonitoringReportState201,
    },
}

/// Typed command evidence: the immutable request and the charger's native reply.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MonitoringResult201 {
    pub query: MonitoringQuery201,
    pub outcome: MonitoringOutcome201,
}

impl MonitoringResult201 {
    /// Whether the charger accepted everything it was asked to do. A report is accepted when the
    /// native acknowledgement is `Accepted`, whatever collection later produced.
    #[must_use]
    pub fn accepted(&self) -> bool {
        match &self.outcome {
            MonitoringOutcome201::SetMonitors { monitors } => {
                !monitors.is_empty() && monitors.iter().all(|item| item.status.accepted())
            }
            MonitoringOutcome201::ClearMonitors { monitors } => {
                !monitors.is_empty() && monitors.iter().all(|item| item.status.accepted())
            }
            MonitoringOutcome201::Base { status } => *status == DeviceReportAck201::Accepted,
            MonitoringOutcome201::Level { status } => *status == MonitoringLevelStatus201::Accepted,
            MonitoringOutcome201::Report { ack, .. } => *ack == Some(DeviceReportAck201::Accepted),
        }
    }

    /// Report collection is still in flight.
    #[must_use]
    pub const fn pending(&self) -> bool {
        match &self.outcome {
            MonitoringOutcome201::Report { report, .. } => report.pending(),
            MonitoringOutcome201::SetMonitors { .. }
            | MonitoringOutcome201::ClearMonitors { .. }
            | MonitoringOutcome201::Base { .. }
            | MonitoringOutcome201::Level { .. } => false,
        }
    }
}

// Serde passes a reference to `skip_serializing_if` predicates.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(value: &u32) -> bool {
    *value == 0
}
