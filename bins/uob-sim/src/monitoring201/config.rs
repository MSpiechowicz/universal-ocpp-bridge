//! Operator-facing OCPP 2.0.1 monitoring options: the seeded device model, the monitoring
//! limits and the fault knobs that make the station misbehave on purpose.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const NAME_LIMIT: usize = 50;
const VALUE_LIMIT: usize = 2500;
const LIMIT_CEILING: u32 = 256;

/// Controllers whose variables the simulator already answers, so a monitored variable cannot
/// shadow them.
const RESERVED_COMPONENTS: [&str; 3] = ["MonitoringCtrlr", "LocalAuthListCtrlr", "AuthCacheCtrlr"];

/// `MonitorEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum MonitorType201 {
    UpperThreshold,
    LowerThreshold,
    Delta,
    Periodic,
    PeriodicClockAligned,
}

impl MonitorType201 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UpperThreshold => "UpperThreshold",
            Self::LowerThreshold => "LowerThreshold",
            Self::Delta => "Delta",
            Self::Periodic => "Periodic",
            Self::PeriodicClockAligned => "PeriodicClockAligned",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        [
            Self::UpperThreshold,
            Self::LowerThreshold,
            Self::Delta,
            Self::Periodic,
            Self::PeriodicClockAligned,
        ]
        .into_iter()
        .find(|candidate| candidate.as_str() == text)
    }

    pub(crate) const fn is_threshold(self) -> bool {
        matches!(self, Self::UpperThreshold | Self::LowerThreshold)
    }

    /// The `MonitoringCriterionEnumType` that selects this monitor type (N02.FR.12-14).
    pub(crate) const fn criterion(self) -> MonitoringCriterion201 {
        match self {
            Self::UpperThreshold | Self::LowerThreshold => {
                MonitoringCriterion201::ThresholdMonitoring
            }
            Self::Delta => MonitoringCriterion201::DeltaMonitoring,
            Self::Periodic | Self::PeriodicClockAligned => {
                MonitoringCriterion201::PeriodicMonitoring
            }
        }
    }
}

/// `MonitoringCriterionEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum MonitoringCriterion201 {
    ThresholdMonitoring,
    DeltaMonitoring,
    PeriodicMonitoring,
}

impl MonitoringCriterion201 {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        [
            Self::ThresholdMonitoring,
            Self::DeltaMonitoring,
            Self::PeriodicMonitoring,
        ]
        .into_iter()
        .find(|candidate| candidate.as_str() == text)
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ThresholdMonitoring => "ThresholdMonitoring",
            Self::DeltaMonitoring => "DeltaMonitoring",
            Self::PeriodicMonitoring => "PeriodicMonitoring",
        }
    }
}

/// `MonitoringBaseEnumType`.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum MonitoringBase201 {
    All,
    FactoryDefault,
    HardWiredOnly,
}

impl MonitoringBase201 {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        [Self::All, Self::FactoryDefault, Self::HardWiredOnly]
            .into_iter()
            .find(|candidate| candidate.as_str() == text)
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::FactoryDefault => "FactoryDefault",
            Self::HardWiredOnly => "HardWiredOnly",
        }
    }
}

/// How the station mistreats a `GetMonitoringReport` it accepted.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReportFault201 {
    #[default]
    None,
    /// Number the second and later fragments one higher, so one `seqNo` is missing.
    SkipSeqNo,
    /// Number every fragment after the first one lower, so one `seqNo` repeats.
    DuplicateSeqNo,
    /// Never send the final fragment, so the CSMS waits for a `tbc` chain that never ends.
    OmitFinalFragment,
    /// Close the socket after the first fragment (before it when there is only one).
    DisconnectMidReport,
    /// Send every fragment before the `GetMonitoringReport` reply.
    FragmentsBeforeReply,
}

/// How the station mistreats a `SetVariableMonitoring` reply.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SetReplyFault201 {
    #[default]
    None,
    /// Answer one result more than there were items (N04.FR.01).
    WrongResultCount,
    /// Name another component in the first result (N04.FR.02).
    MismatchedComponent,
}

/// `EVSEType`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvseId201 {
    pub id: u32,
    #[serde(default, rename = "connectorId")]
    pub connector_id: Option<u32>,
}

/// `ComponentType`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComponentId201 {
    pub name: String,
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub evse: Option<EvseId201>,
}

/// `VariableType`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VariableId201 {
    pub name: String,
    #[serde(default)]
    pub instance: Option<String>,
}

impl ComponentId201 {
    pub(crate) fn to_json(&self) -> Value {
        let mut component = json!({"name": self.name});
        if let Some(instance) = &self.instance {
            component["instance"] = json!(instance);
        }
        if let Some(evse) = &self.evse {
            component["evse"] = json!({"id": evse.id});
            if let Some(connector) = evse.connector_id {
                component["evse"]["connectorId"] = json!(connector);
            }
        }
        component
    }
}

impl VariableId201 {
    pub(crate) fn to_json(&self) -> Value {
        let mut variable = json!({"name": self.name});
        if let Some(instance) = &self.instance {
            variable["instance"] = json!(instance);
        }
        variable
    }
}

/// The `dataType` a monitored variable reports in its characteristics.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VariableType201 {
    #[default]
    Decimal,
    Integer,
    Boolean,
    String,
}

impl VariableType201 {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Decimal => "decimal",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::String => "string",
        }
    }

    pub(crate) const fn numeric(self) -> bool {
        matches!(self, Self::Decimal | Self::Integer)
    }
}

/// One variable of the seeded device model.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitoredVariable201 {
    pub component: ComponentId201,
    pub variable: VariableId201,
    #[serde(default)]
    pub data_type: VariableType201,
    /// Initial `Actual` value; scenario steps change it afterwards.
    pub value: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Monitor types the variable supports. Empty means the variable cannot be monitored.
    #[serde(default)]
    pub monitor_types: Vec<MonitorType201>,
}

/// Who owns a seeded monitor: firmware (`HardWiredMonitor`) or a factory preset
/// (`PreconfiguredMonitor`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SeededKind201 {
    HardWired,
    Preconfigured,
}

/// One monitor the station owns before any `SetVariableMonitoring`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeededMonitor201 {
    pub id: i32,
    pub kind: SeededKind201,
    pub component: ComponentId201,
    pub variable: VariableId201,
    #[serde(rename = "type")]
    pub monitor_type: MonitorType201,
    pub value: f64,
    pub severity: u8,
    #[serde(default)]
    pub transaction: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Monitoring201Config {
    /// `ItemsPerMessage` / `SetVariableMonitoring`.
    #[serde(default = "default_items")]
    pub items_per_message_set: u32,
    /// `BytesPerMessage` / `SetVariableMonitoring`.
    #[serde(default = "default_set_bytes")]
    pub bytes_per_message_set: u32,
    /// `ItemsPerMessage` / `ClearVariableMonitoring`.
    #[serde(default = "default_items")]
    pub items_per_message_clear: u32,
    /// `BytesPerMessage` / `ClearVariableMonitoring`.
    #[serde(default = "default_clear_bytes")]
    pub bytes_per_message_clear: u32,
    /// Most monitors the station holds at once (`ActiveMonitors` maxLimit).
    #[serde(default = "default_max_monitors")]
    pub max_monitors: u32,
    /// Monitors in one `NotifyMonitoringReport`; a report with more monitors is fragmented.
    #[serde(default = "default_per_message")]
    pub monitors_per_fragment: u32,
    /// Events in one `NotifyEvent`; a burst with more events is chained with `tbc`.
    #[serde(default = "default_per_message")]
    pub events_per_message: u32,
    /// Severity level before any `SetMonitoringLevel` (N05.FR.03).
    #[serde(default = "default_level")]
    pub initial_level: u8,
    #[serde(default = "all_criteria")]
    pub supported_criteria: Vec<MonitoringCriterion201>,
    #[serde(default = "all_bases")]
    pub supported_bases: Vec<MonitoringBase201>,
    #[serde(default)]
    pub report_fault: ReportFault201,
    #[serde(default)]
    pub set_reply_fault: SetReplyFault201,
    #[serde(default)]
    pub variables: Vec<MonitoredVariable201>,
    #[serde(default)]
    pub monitors: Vec<SeededMonitor201>,
}

impl Monitoring201Config {
    /// # Errors
    /// Rejects limits outside the simulator's ranges and seeded monitors the seeded device model
    /// could not have produced.
    pub fn validate(&self) -> Result<(), &'static str> {
        let limits = [
            self.items_per_message_set,
            self.items_per_message_clear,
            self.max_monitors,
            self.monitors_per_fragment,
            self.events_per_message,
        ];
        if limits
            .iter()
            .any(|limit| !(1..=LIMIT_CEILING).contains(limit))
            || self.bytes_per_message_set < 64
            || self.bytes_per_message_clear < 16
            || self.initial_level > 9
            || self.variables.len() > 64
            || self.monitors.len() > usize::try_from(self.max_monitors).unwrap_or(0)
        {
            return Err("monitoring_config_invalid");
        }
        for (index, variable) in self.variables.iter().enumerate() {
            let duplicate = self.variables[..index]
                .iter()
                .any(|earlier| same_identity(earlier, variable));
            if duplicate || !valid_variable(variable) {
                return Err("monitoring_config_invalid");
            }
        }
        for (index, monitor) in self.monitors.iter().enumerate() {
            let duplicate = self.monitors[..index]
                .iter()
                .any(|earlier| earlier.id == monitor.id);
            if duplicate || !self.valid_monitor(monitor) {
                return Err("monitoring_config_invalid");
            }
        }
        Ok(())
    }

    fn valid_monitor(&self, monitor: &SeededMonitor201) -> bool {
        let Some(variable) = self.variables.iter().find(|variable| {
            super::model::same_component(&variable.component, &monitor.component)
                && super::model::same_variable(&variable.variable, &monitor.variable)
        }) else {
            return false;
        };
        monitor.id > 0
            && monitor.severity <= 9
            && variable.monitor_types.contains(&monitor.monitor_type)
            && super::model::acceptable_value(
                monitor.monitor_type,
                monitor.value,
                variable.min,
                variable.max,
            )
            .is_ok()
    }
}

fn same_identity(left: &MonitoredVariable201, right: &MonitoredVariable201) -> bool {
    super::model::same_component(&left.component, &right.component)
        && super::model::same_variable(&left.variable, &right.variable)
}

fn valid_variable(variable: &MonitoredVariable201) -> bool {
    let names = [
        Some(variable.component.name.as_str()),
        variable.component.instance.as_deref(),
        Some(variable.variable.name.as_str()),
        variable.variable.instance.as_deref(),
    ];
    let reserved = RESERVED_COMPONENTS
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&variable.component.name));
    let numeric_value =
        !variable.data_type.numeric() || variable.value.parse::<f64>().is_ok_and(f64::is_finite);
    let boolean_value = variable.data_type != VariableType201::Boolean
        || matches!(variable.value.as_str(), "true" | "false");
    let bounds = variable.min.is_none_or(f64::is_finite)
        && variable.max.is_none_or(f64::is_finite)
        && variable
            .min
            .zip(variable.max)
            .is_none_or(|(minimum, maximum)| minimum <= maximum);
    let evse = variable
        .component
        .evse
        .as_ref()
        .is_none_or(|evse| evse.id > 0 && evse.connector_id.is_none_or(|id| id > 0));
    !reserved
        && evse
        && bounds
        && numeric_value
        && boolean_value
        && names
            .into_iter()
            .flatten()
            .all(|name| !name.is_empty() && name.chars().count() <= NAME_LIMIT)
        && variable.value.chars().count() <= VALUE_LIMIT
}

const fn default_items() -> u32 {
    8
}

const fn default_set_bytes() -> u32 {
    16_384
}

const fn default_clear_bytes() -> u32 {
    4096
}

const fn default_max_monitors() -> u32 {
    32
}

const fn default_per_message() -> u32 {
    4
}

const fn default_level() -> u8 {
    9
}

fn all_criteria() -> Vec<MonitoringCriterion201> {
    vec![
        MonitoringCriterion201::ThresholdMonitoring,
        MonitoringCriterion201::DeltaMonitoring,
        MonitoringCriterion201::PeriodicMonitoring,
    ]
}

fn all_bases() -> Vec<MonitoringBase201> {
    vec![
        MonitoringBase201::All,
        MonitoringBase201::FactoryDefault,
        MonitoringBase201::HardWiredOnly,
    ]
}
