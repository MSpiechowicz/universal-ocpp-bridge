//! Credential-free catalog, backlog, and configuration views.
use std::collections::BTreeMap;

use uob_application::{ConfigurationFieldKind, TargetBacklogFact, TargetDeliveryDestination};
use uob_contracts::{BridgeId, Environment};
use uob_management_adapter::{
    BacklogView, DestinationView, NamedView, SchemaFieldView, SettingView, TargetCatalogView,
    TargetEntryView, TargetKindView,
};
use uob_target_adapter::{TargetCatalogEntry, TransportPolicy};

use super::{
    ConfigurationLoadError, TargetEntry, is_credential_field, target_issues::bounded_label,
    target_registry, targets::MAX_SETTING_BYTES,
};

pub(crate) fn destination_view(destination: &TargetDeliveryDestination) -> DestinationView {
    DestinationView {
        target_id: destination.target_instance_id.as_str().to_owned(),
        configuration_revision: destination.configuration_revision,
    }
}

pub(crate) fn backlog_view(fact: &TargetBacklogFact) -> BacklogView {
    BacklogView {
        target_id: fact.destination.target_instance_id.as_str().to_owned(),
        configuration_revision: fact.destination.configuration_revision,
        pending_critical_deliveries: fact.pending_critical_deliveries,
        pending_deliveries: fact.pending_deliveries,
    }
}

/// Credential-free catalog from the same registry startup validates with.
pub(crate) fn catalog_view(
    bridge_id: &BridgeId,
    environment: Environment,
) -> Result<TargetCatalogView, ConfigurationLoadError> {
    let kinds = target_registry(bridge_id, environment)?
        .catalog()
        .into_iter()
        .map(|entry| TargetKindView {
            kind: entry.kind.as_str().to_owned(),
            display_family: NamedView {
                id: entry.display_family.id,
                display_name: entry.display_family.display_name,
            },
            configuration_schema: entry
                .configuration_schema
                .fields
                .into_iter()
                .map(|field| SchemaFieldView {
                    name: field.name,
                    kind: field_kind(field.kind),
                    required: field.required,
                })
                .collect(),
            presets: entry
                .presets
                .into_iter()
                .map(|preset| NamedView {
                    id: preset.id,
                    display_name: preset.display_name,
                })
                .collect(),
            capabilities: entry
                .capabilities
                .into_iter()
                .map(|value| value.0)
                .collect(),
            transport_policy: entry.transport_policy.map(|policy| match policy {
                TransportPolicy::Outbound => "outbound",
                TransportPolicy::Listener => "listener",
            }),
            available: entry.available,
        })
        .collect();
    Ok(TargetCatalogView { kinds })
}

const fn field_kind(kind: ConfigurationFieldKind) -> &'static str {
    match kind {
        ConfigurationFieldKind::Text => "text",
        ConfigurationFieldKind::Integer => "integer",
        ConfigurationFieldKind::Boolean => "boolean",
        ConfigurationFieldKind::CredentialReference => "credential_reference",
    }
}

/// Only schema-declared settings are shown; credential fields as references; anything that could
/// embed secret material (undeclared names, URL userinfo, unbounded text) is withheld.
pub(super) fn entry_view(entry: &TargetEntry, catalog: &[TargetCatalogEntry]) -> TargetEntryView {
    let declared = catalog
        .iter()
        .find(|kind| kind.kind.as_str() == entry.kind)
        .map(|kind| {
            kind.configuration_schema
                .fields
                .iter()
                .map(|field| (field.name.as_str(), field.kind))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let settings = entry
        .settings
        .iter()
        .map(|(name, value)| {
            let view = match (declared.get(name.as_str()), value) {
                (Some(ConfigurationFieldKind::CredentialReference), toml::Value::String(value))
                    if is_credential_field(name) =>
                {
                    SettingView::CredentialReference {
                        credential_reference: value.clone(),
                    }
                }
                (Some(ConfigurationFieldKind::Text), toml::Value::String(value))
                    if value.len() <= MAX_SETTING_BYTES
                        && !value.contains('@')
                        && !value.chars().any(char::is_control) =>
                {
                    SettingView::Text(value.clone())
                }
                (Some(ConfigurationFieldKind::Integer), toml::Value::Integer(value)) => {
                    SettingView::Integer(*value)
                }
                (Some(ConfigurationFieldKind::Boolean), toml::Value::Boolean(value)) => {
                    SettingView::Boolean(*value)
                }
                _ => SettingView::Redacted { redacted: true },
            };
            (name.clone(), view)
        })
        .collect();
    let preset = match (declared.get("profile"), entry.settings.get("profile")) {
        (Some(ConfigurationFieldKind::Text), Some(toml::Value::String(profile))) => {
            Some(bounded_label(profile))
        }
        _ => None,
    };
    TargetEntryView {
        id: entry.id.clone(),
        kind: entry.kind.clone(),
        enabled: entry.enabled,
        revision: entry.revision,
        preset,
        settings,
    }
}
