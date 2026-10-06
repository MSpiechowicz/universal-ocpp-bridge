//! Credential-free request and response shapes of the target configuration API.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uob_contracts::{Environment, UtcTimestamp};

/// Installed target kinds, built from the same registry the service validates with.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetCatalogView {
    /// Every registered or explicitly unavailable kind.
    pub kinds: Vec<TargetKindView>,
}

/// One registry kind.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetKindView {
    /// Stable factory kind, such as `ems-scada.http`.
    pub kind: String,
    /// Display grouping independent of the concrete kind.
    pub display_family: NamedView,
    /// Safe field declarations; credential fields accept references only.
    pub configuration_schema: Vec<SchemaFieldView>,
    /// Presets implemented by the same factory, never a second target.
    pub presets: Vec<NamedView>,
    /// Declared capabilities.
    pub capabilities: Vec<String>,
    /// Shared transport policy, when the kind has one.
    pub transport_policy: Option<&'static str>,
    /// Whether this executable can construct the kind.
    pub available: bool,
}

/// Stable identifier with a human-readable label.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NamedView {
    /// Stable identifier.
    pub id: String,
    /// Human-readable label.
    pub display_name: String,
}

/// One declared configuration field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SchemaFieldView {
    /// Setting name.
    pub name: String,
    /// `text`, `integer`, `boolean`, or `credential_reference`.
    pub kind: &'static str,
    /// Whether validation requires the field.
    pub required: bool,
}

/// Exact owner of target work.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DestinationView {
    /// Configured target instance.
    pub target_id: String,
    /// Configuration revision stamped on its work.
    pub configuration_revision: u64,
}

/// Pending outbox work of one destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BacklogView {
    /// Configured target instance.
    pub target_id: String,
    /// Configuration revision stamped on the work.
    pub configuration_revision: u64,
    /// Critical deliveries awaiting a terminal outcome.
    pub pending_critical_deliveries: u64,
    /// All pending deliveries, including replaceable telemetry.
    pub pending_deliveries: u64,
}

/// Audited archive or discard authorization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetDispositionView {
    /// Durable audit event; the proof target-change validation accepts.
    pub audit_event_id: String,
    /// Old target instance covered by the event.
    pub target_id: String,
    /// Old configuration revision covered by the event.
    pub configuration_revision: u64,
    /// `archive` or `discard`.
    pub action: DispositionAction,
    /// `authorized` until the next start executes or supersedes it.
    pub state: &'static str,
    /// Authenticated management principal that authorized it.
    pub authorized_by: String,
    /// Trusted authorization time.
    pub authorized_at: UtcTimestamp,
}

/// Value of one setting. Credentials appear only as references; never as secret material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SettingView {
    /// Boolean flag.
    Boolean(bool),
    /// Signed integer.
    Integer(i64),
    /// Non-secret text.
    Text(String),
    /// Location or name of a credential resolved only when the target starts.
    CredentialReference {
        /// Reference, such as a protected file path.
        credential_reference: String,
    },
    /// Text withheld because it could embed credential material.
    Redacted {
        /// Always `true`.
        redacted: bool,
    },
}

/// One declared target instance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetEntryView {
    /// Stable instance identifier.
    pub id: String,
    /// Registry kind.
    pub kind: String,
    /// Whether this instance is the selected target.
    pub enabled: bool,
    /// Configuration revision bound to its work.
    pub revision: u64,
    /// Selected preset/profile, when the instance names one.
    pub preset: Option<String>,
    /// Settings with credentials shown only as references.
    pub settings: BTreeMap<String, SettingView>,
}

/// Target section that the next service start will use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetSectionView {
    /// `base` for the startup file, `staged` after an API apply.
    pub source: &'static str,
    /// Selected instance, if any.
    pub target_id: Option<String>,
    /// Declared instances.
    pub targets: Vec<TargetEntryView>,
}

/// Current and next-start target configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetConfigurationView {
    /// Trusted bridge identity.
    pub bridge_id: String,
    /// Trusted runtime environment.
    pub environment: Environment,
    /// Digest of the next-start section; required by apply for optimistic concurrency.
    pub configuration_digest: String,
    /// Destination of the running target session, if any.
    pub running_destination: Option<DestinationView>,
    /// Destination the next start will select, if any.
    pub next_destination: Option<DestinationView>,
    /// Whether the next start changes the running destination.
    pub restart_required: bool,
    /// Next-start target section.
    pub next_start: TargetSectionView,
    /// Pending outbox work by destination.
    pub backlog: Vec<BacklogView>,
    /// Authorized dispositions awaiting the next start.
    pub dispositions: Vec<TargetDispositionView>,
}

/// Candidate target section. Revisions are assigned by the service.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TargetConfigurationCandidate {
    /// Instance to select.
    pub target_id: String,
    /// Declared instances; exactly one must be enabled.
    pub targets: Vec<CandidateTarget>,
}

/// One candidate instance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CandidateTarget {
    /// Stable instance identifier.
    pub id: String,
    /// Registry kind.
    pub kind: String,
    /// Whether this instance is selected.
    #[serde(default)]
    pub enabled: bool,
    /// Typed settings; `*_file` and `*credential*` names hold references.
    #[serde(default)]
    pub settings: BTreeMap<String, CandidateSetting>,
}

/// One candidate setting value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum CandidateSetting {
    /// Boolean flag.
    Boolean(bool),
    /// Signed integer.
    Integer(i64),
    /// Text or, for credential fields, a reference path/name.
    Text(String),
    /// Explicit credential reference, as returned by the configuration view.
    CredentialReference {
        /// Reference, such as a protected file path.
        credential_reference: String,
    },
}

/// Apply request guarded by the digest of the section it replaces.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TargetApplyRequest {
    /// `configuration_digest` from the view the client edited.
    pub expected_digest: String,
    /// Candidate target section.
    pub configuration: TargetConfigurationCandidate,
}

/// Archive or discard.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DispositionAction {
    /// Retain payloads without dispatch.
    Archive,
    /// Permanently remove payloads.
    Discard,
}

/// Request to authorize terminal handling of one exact old destination.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TargetDispositionRequest {
    /// Old target instance.
    pub target_id: String,
    /// Old configuration revision.
    pub configuration_revision: u64,
    /// Archive or discard.
    pub action: DispositionAction,
}

/// One sanitized validation failure: codes and field names, never rejected values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ValidationIssue {
    /// Stable failure code.
    pub code: String,
    /// Target instance involved, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    /// Schema field involved, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// Result of validating a candidate against the persisted section and durable backlog.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetValidationReport {
    /// Whether apply would accept the candidate.
    pub valid: bool,
    /// Sanitized failures; empty when valid.
    pub errors: Vec<ValidationIssue>,
    /// Digest of the persisted section the candidate was compared with.
    pub configuration_digest: String,
    /// Service-assigned revision of every candidate instance.
    pub assigned_revisions: BTreeMap<String, u64>,
    /// Destination of the running target session, if any.
    pub running_destination: Option<DestinationView>,
    /// Destination the candidate selects, when it could be determined.
    pub next_destination: Option<DestinationView>,
    /// Whether applying the candidate requires a restart to take effect.
    pub restart_required: bool,
    /// Critical work pending across all destinations.
    pub pending_critical_deliveries: u64,
    /// Old destinations whose critical work blocks the change without a disposition.
    pub blocking_destinations: Vec<BacklogView>,
    /// Audit events covering old destinations that would otherwise block the change.
    pub dispositions: Vec<String>,
}

/// Successful next-start apply.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetApplyOutcome {
    /// Digest of the newly persisted section.
    pub configuration_digest: String,
    /// Destination of the running target session, unchanged by apply.
    pub running_destination: Option<DestinationView>,
    /// Destination selected after the next start.
    pub next_destination: DestinationView,
    /// Whether a service restart is required to use it.
    pub restart_required: bool,
    /// Audit events the next start executes for old destinations.
    pub dispositions: Vec<String>,
}
