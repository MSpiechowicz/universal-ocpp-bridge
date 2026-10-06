//! Next-start target sections for the configuration API.
//!
//! The startup file stays read-only (`ProtectSystem=strict`). An API apply writes a private
//! staged section that replaces the file's `bridge.target_id` and `[[targets]]` on the next load,
//! but only while the file's own target section still matches the digest it was derived from.
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use uob_application::{
    DeliveryDispositionAction, DeliveryDispositionRecord, TargetBacklogFact,
    TargetDeliveryDestination,
};
use uob_contracts::{BridgeId, Environment, TargetInstanceId};
use uob_management_adapter::{
    CandidateSetting, TargetConfigurationCandidate, TargetSectionView, TargetValidationReport,
    ValidationIssue,
};
use uob_target_adapter::{
    AuditedTargetDisposition, TargetBacklogEntry, TargetBacklogState, TargetDestination,
    TargetDispositionAction,
};

pub(crate) use super::target_views::{backlog_view, catalog_view, destination_view};
use super::{
    ConfigurationLoadError, FileConfiguration, TargetEntry, is_credential_field, read_document,
    staged,
    target_issues::{change_issue, issue},
    target_registry, target_selection,
    target_views::entry_view,
    validate,
};

pub(super) const MAX_CANDIDATE_TARGETS: usize = 16;
const MAX_CANDIDATE_SETTINGS: usize = 32;
pub(super) const MAX_SETTING_BYTES: usize = 1024;

/// `bridge.target_id` plus `[[targets]]`.
#[derive(Clone, PartialEq)]
pub(crate) struct TargetSection {
    pub(super) target_id: Option<String>,
    pub(super) targets: Vec<TargetEntry>,
}

impl TargetSection {
    pub(super) fn of(configuration: &FileConfiguration) -> Self {
        Self {
            target_id: configuration.bridge.target_id.clone(),
            targets: configuration.targets.clone(),
        }
    }

    /// Canonical digest used for staged-overlay binding and optimistic apply concurrency.
    pub(crate) fn digest(&self) -> String {
        #[derive(Serialize)]
        struct Canonical<'a> {
            target_id: &'a Option<String>,
            targets: &'a [TargetEntry],
        }
        let encoded = serde_json::to_vec(&Canonical {
            target_id: &self.target_id,
            targets: &self.targets,
        })
        .expect("target sections contain only serializable TOML values");
        let mut digest = String::with_capacity(71);
        digest.push_str("sha256:");
        for byte in Sha256::digest(encoded) {
            use std::fmt::Write as _;
            write!(&mut digest, "{byte:02x}").expect("writing to a String cannot fail");
        }
        digest
    }

    /// Destination of the selected, enabled instance, if the section names one.
    pub(crate) fn destination(&self) -> Option<TargetDeliveryDestination> {
        let target_id = self.target_id.as_deref()?;
        let entry = self
            .targets
            .iter()
            .find(|entry| entry.enabled && entry.id == target_id)?;
        Some(TargetDeliveryDestination {
            target_instance_id: TargetInstanceId::new(target_id).ok()?,
            configuration_revision: entry.revision,
        })
    }
}

/// Startup file and optional staged overlay, read fresh for every API operation.
pub(crate) struct Sections {
    document: FileConfiguration,
    base: TargetSection,
    staged: Option<TargetSection>,
    staged_path: Option<PathBuf>,
}

impl Sections {
    pub(crate) fn read(path: &Path) -> Result<Self, ConfigurationLoadError> {
        let document = read_document(path)?;
        let base = TargetSection::of(&document);
        let staged_path = document
            .configuration_api
            .staged_targets_file()
            .map(Path::to_path_buf);
        let staged = match &staged_path {
            Some(staged_path) => staged::read(staged_path, &base)?,
            None => None,
        };
        Ok(Self {
            document,
            base,
            staged,
            staged_path,
        })
    }

    /// Section the next start loads.
    pub(crate) fn effective(&self) -> &TargetSection {
        self.staged.as_ref().unwrap_or(&self.base)
    }

    fn environment(&self) -> Environment {
        self.document.bridge.environment
    }

    fn bridge_id(&self) -> Result<BridgeId, ConfigurationLoadError> {
        BridgeId::new(self.document.bridge.id.clone())
            .map_err(|_| ConfigurationLoadError::InvalidIdentity)
    }

    /// Sanitized view of the next-start section.
    pub(crate) fn view(&self) -> TargetSectionView {
        let catalog = self
            .bridge_id()
            .and_then(|bridge| target_registry(&bridge, self.environment()))
            .map(|registry| registry.catalog())
            .unwrap_or_default();
        let section = self.effective();
        TargetSectionView {
            source: if self.staged.is_some() {
                "staged"
            } else {
                "base"
            },
            target_id: section.target_id.clone(),
            targets: section
                .targets
                .iter()
                .map(|entry| entry_view(entry, &catalog))
                .collect(),
        }
    }
}

/// Durable facts gathered by the host before evaluating a candidate.
pub(crate) struct DurableFacts<'a> {
    pub running: Option<&'a TargetDeliveryDestination>,
    pub backlog: &'a [TargetBacklogFact],
    pub dispositions: &'a [DeliveryDispositionRecord],
}

/// Candidate evaluation, including the section apply would persist.
pub(crate) struct Evaluation {
    pub report: TargetValidationReport,
    candidate: Option<TargetSection>,
    base_digest: String,
    staged_path: Option<PathBuf>,
}

/// Validates a candidate with the same shared rules as `uob config check` plus the durable
/// backlog and audited dispositions accepted by the target-change preview.
pub(crate) fn evaluate(
    sections: Sections,
    candidate: &TargetConfigurationCandidate,
    facts: &DurableFacts<'_>,
) -> Evaluation {
    let configuration_digest = sections.effective().digest();
    let base_digest = sections.base.digest();
    let staged_path = sections.staged_path.clone();
    let mut report = TargetValidationReport {
        valid: false,
        errors: vec![],
        configuration_digest,
        assigned_revisions: BTreeMap::new(),
        running_destination: facts.running.map(destination_view),
        next_destination: None,
        restart_required: false,
        pending_critical_deliveries: facts
            .backlog
            .iter()
            .map(|fact| fact.pending_critical_deliveries)
            .fold(0, u64::saturating_add),
        blocking_destinations: vec![],
        dispositions: vec![],
    };
    let section = match candidate_section(candidate, sections.effective(), facts) {
        Ok(section) => section,
        Err(issues) => {
            report.errors = issues;
            return Evaluation {
                report,
                candidate: None,
                base_digest,
                staged_path,
            };
        }
    };
    report.assigned_revisions = section
        .targets
        .iter()
        .map(|entry| (entry.id.clone(), entry.revision))
        .collect();
    let next = section.destination();
    report.next_destination = next.as_ref().map(destination_view);
    report.restart_required = next.is_some() && facts.running != next.as_ref();
    check_backlog(&mut report, next.as_ref(), facts);
    check_preview(&mut report, &sections, &section, facts);
    if report.errors.is_empty() {
        // The full shared document validation (staging isolation, listener rules, identity and
        // composition) runs exactly as `uob config check` would on the next start.
        let mut document = sections.document;
        document.bridge.target_id.clone_from(&section.target_id);
        document.targets.clone_from(&section.targets);
        if let Err(error) = validate(document) {
            report.errors.push(issue(error.code(), None, None));
        }
    }
    report.valid = report.errors.is_empty();
    Evaluation {
        candidate: report.valid.then_some(section),
        report,
        base_digest,
        staged_path,
    }
}

impl Evaluation {
    /// Service-writable location of the next-start section, if apply is enabled.
    pub(crate) fn staged_path(&self) -> Option<&Path> {
        self.staged_path.as_deref()
    }

    /// Atomically persists the valid candidate as the next-start section.
    ///
    /// A candidate identical to the startup file removes the staged overlay instead.
    pub(crate) fn persist(
        &self,
        path: &Path,
        actor: &str,
        applied_at: &str,
    ) -> io::Result<Option<String>> {
        let Some(section) = &self.candidate else {
            return Ok(None);
        };
        let digest = section.digest();
        if digest == self.base_digest {
            staged::remove(path)?;
        } else {
            staged::write(path, &self.base_digest, section, actor, applied_at)?;
        }
        Ok(Some(digest))
    }
}

fn candidate_section(
    candidate: &TargetConfigurationCandidate,
    current: &TargetSection,
    facts: &DurableFacts<'_>,
) -> Result<TargetSection, Vec<ValidationIssue>> {
    if candidate.targets.len() > MAX_CANDIDATE_TARGETS {
        return Err(vec![issue("configuration.too_many_targets", None, None)]);
    }
    let mut issues = vec![];
    let mut targets = Vec::with_capacity(candidate.targets.len());
    for target in &candidate.targets {
        if target.settings.len() > MAX_CANDIDATE_SETTINGS {
            issues.push(issue(
                "configuration.too_many_settings",
                Some(&target.id),
                None,
            ));
            continue;
        }
        let mut settings = BTreeMap::new();
        for (name, value) in &target.settings {
            match setting_value(name, value) {
                Some(value) => {
                    settings.insert(name.clone(), value);
                }
                None => issues.push(issue(
                    "configuration.invalid_target_setting",
                    Some(&target.id),
                    Some(name),
                )),
            }
        }
        let mut entry = TargetEntry {
            id: target.id.clone(),
            kind: target.kind.clone(),
            enabled: target.enabled,
            revision: 0,
            settings,
            transport: None,
        };
        entry.revision = assign_revision(&entry, current, facts);
        targets.push(entry);
    }
    if issues.is_empty() {
        Ok(TargetSection {
            target_id: Some(candidate.target_id.clone()),
            targets,
        })
    } else {
        Err(issues)
    }
}

fn setting_value(name: &str, value: &CandidateSetting) -> Option<toml::Value> {
    let bounded = |text: &str| text.len() <= MAX_SETTING_BYTES;
    match value {
        CandidateSetting::Boolean(value) => Some(toml::Value::Boolean(*value)),
        CandidateSetting::Integer(value) => Some(toml::Value::Integer(*value)),
        CandidateSetting::Text(value) if bounded(value) => Some(toml::Value::String(value.clone())),
        CandidateSetting::CredentialReference {
            credential_reference,
        } if is_credential_field(name) && bounded(credential_reference) => {
            Some(toml::Value::String(credential_reference.clone()))
        }
        _ => None,
    }
}

/// Keeps an unchanged instance's revision; any change gets a revision above every revision the
/// instance has used for pending or audited work, so old work can never match the new owner.
fn assign_revision(entry: &TargetEntry, current: &TargetSection, facts: &DurableFacts<'_>) -> u64 {
    let existing = current.targets.iter().find(|other| other.id == entry.id);
    if let Some(existing) = existing
        && existing.kind == entry.kind
        && existing.settings == entry.settings
        && existing.transport == entry.transport
    {
        return existing.revision;
    }
    let used = |destination: &TargetDeliveryDestination| {
        (destination.target_instance_id.as_str() == entry.id)
            .then_some(destination.configuration_revision)
    };
    existing
        .map(|existing| existing.revision)
        .into_iter()
        .chain(facts.running.and_then(used))
        .chain(
            facts
                .backlog
                .iter()
                .filter_map(|fact| used(&fact.destination)),
        )
        .chain(
            facts
                .dispositions
                .iter()
                .filter_map(|record| used(&record.destination)),
        )
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

/// Old owners with pending critical work either carry an authorized disposition or block.
fn check_backlog(
    report: &mut TargetValidationReport,
    next: Option<&TargetDeliveryDestination>,
    facts: &DurableFacts<'_>,
) {
    for fact in facts.backlog {
        if fact.pending_critical_deliveries == 0 || Some(&fact.destination) == next {
            continue;
        }
        match facts
            .dispositions
            .iter()
            .find(|record| record.destination == fact.destination)
        {
            Some(record) => report.dispositions.push(record.audit_event_id.clone()),
            None => report.blocking_destinations.push(backlog_view(fact)),
        }
    }
}

fn check_preview(
    report: &mut TargetValidationReport,
    sections: &Sections,
    section: &TargetSection,
    facts: &DurableFacts<'_>,
) {
    let selected = section.target_id.clone().unwrap_or_default();
    let bridge_id = match sections.bridge_id() {
        Ok(bridge_id) => bridge_id,
        Err(error) => return report.errors.push(issue(error.code(), None, None)),
    };
    let registry = match target_registry(&bridge_id, sections.environment()) {
        Ok(registry) => registry,
        Err(error) => return report.errors.push(issue(error.code(), None, None)),
    };
    let selection = match target_selection(
        &bridge_id,
        sections.environment(),
        section.target_id.clone(),
        section.targets.clone(),
    ) {
        Ok(Some(selection)) => selection,
        Ok(None) => {
            return report
                .errors
                .push(issue("configuration.missing_target_selection", None, None));
        }
        Err(error) => {
            return report
                .errors
                .push(issue(error.code(), Some(&selected), None));
        }
    };
    let backlog = TargetBacklogState {
        entries: facts
            .backlog
            .iter()
            .filter(|fact| fact.pending_critical_deliveries != 0)
            .map(|fact| TargetBacklogEntry {
                destination: target_destination(&fact.destination),
                pending_critical_deliveries: fact.pending_critical_deliveries,
            })
            .collect(),
    };
    let proofs = facts
        .dispositions
        .iter()
        .filter(|record| report.dispositions.contains(&record.audit_event_id))
        .map(|record| AuditedTargetDisposition {
            audit_event_id: record.audit_event_id.clone(),
            destination: target_destination(&record.destination),
            action: match record.action {
                DeliveryDispositionAction::Archive => TargetDispositionAction::Archive,
                DeliveryDispositionAction::Discard => TargetDispositionAction::Discard,
            },
        })
        .collect::<Vec<_>>();
    let running = facts.running.map(target_destination);
    if let Err(error) = registry.preview_change(selection, running.as_ref(), &backlog, &proofs) {
        report.errors.push(change_issue(error, section, &selected));
    }
}

fn target_destination(destination: &TargetDeliveryDestination) -> TargetDestination {
    TargetDestination {
        target_instance_id: destination.target_instance_id.clone(),
        configuration_revision: destination.configuration_revision,
    }
}

#[cfg(test)]
#[path = "targets_tests.rs"]
mod tests;
