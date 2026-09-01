use serde::Serialize;

use super::SCHEMA_VERSION;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    Setup,
    Assertion,
    Timeout,
    Cancelled,
}

impl FailureCategory {
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Setup => 2,
            Self::Assertion => 3,
            Self::Timeout => 4,
            Self::Cancelled => 5,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunFailure {
    pub category: FailureCategory,
    pub code: &'static str,
    pub message: String,
}

impl RunFailure {
    #[must_use]
    pub fn new(category: FailureCategory, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            category,
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RunEvent {
    pub schema_version: u16,
    pub sequence: u64,
    pub id: String,
    pub event: &'static str,
    pub status: &'static str,
    pub seed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_category: Option<FailureCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<DiagnosticCounts>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DiagnosticCounts {
    pub rejected_steps: u64,
    pub rejected_commands: u64,
    pub dropped_traces: u64,
}

impl DiagnosticCounts {
    pub fn merge(&mut self, other: Self) {
        self.rejected_steps = self.rejected_steps.saturating_add(other.rejected_steps);
        self.rejected_commands = self
            .rejected_commands
            .saturating_add(other.rejected_commands);
        self.dropped_traces = self.dropped_traces.saturating_add(other.dropped_traces);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunReport {
    pub seed: u64,
    pub events: Vec<RunEvent>,
    pub failure: Option<RunFailure>,
    pub diagnostics: DiagnosticCounts,
    next_sequence: u64,
}

impl RunReport {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            events: Vec::new(),
            failure: None,
            diagnostics: DiagnosticCounts::default(),
            next_sequence: 0,
        }
    }

    #[must_use]
    pub fn failed(seed: u64, failure: RunFailure) -> Self {
        let mut report = Self::new(seed);
        report.finish_failure(failure);
        report
    }

    pub fn push(
        &mut self,
        event: &'static str,
        status: &'static str,
        step_id: Option<&str>,
        station_id: Option<&str>,
        action: Option<&'static str>,
        detail: Option<String>,
    ) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.events.push(RunEvent {
            schema_version: SCHEMA_VERSION,
            sequence,
            id: format!("{:016x}-{sequence:08x}", self.seed),
            event,
            status,
            seed: self.seed,
            step_id: step_id.map(str::to_owned),
            station_id: station_id.map(str::to_owned),
            action,
            detail,
            failure_category: None,
            failure_code: None,
            diagnostics: None,
        });
    }

    pub fn finish_success(&mut self) {
        self.push_summary("run_passed", "passed", None);
    }

    pub fn finish_failure(&mut self, failure: RunFailure) {
        self.push_summary("run_failed", "failed", Some(&failure));
        self.failure = Some(failure);
    }

    fn push_summary(
        &mut self,
        event: &'static str,
        status: &'static str,
        failure: Option<&RunFailure>,
    ) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.events.push(RunEvent {
            schema_version: SCHEMA_VERSION,
            sequence,
            id: format!("{:016x}-{sequence:08x}", self.seed),
            event,
            status,
            seed: self.seed,
            step_id: None,
            station_id: None,
            action: None,
            detail: failure.map(|failure| failure.message.clone()),
            failure_category: failure.map(|failure| failure.category),
            failure_code: failure.map(|failure| failure.code),
            diagnostics: Some(self.diagnostics),
        });
    }
}
