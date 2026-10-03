use super::device_model_values::{component, variable};
use crate::command_registry::{
    configuration201::{self as registry, Request},
    device_model201::identity_key,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use uob_contracts::{
    ConfigurationResult201, DeviceAttributeType201, ResourceRef, SetNetworkProfileStatus201,
    SetVariableResult201, SetVariableStatus201,
};

pub(super) fn response(
    request: &Request,
    payload: &Value,
    resource: &ResourceRef,
) -> Option<ConfigurationResult201> {
    match request {
        Request::Variables(request) => {
            if !registry::valid_schema(1, payload) {
                return None;
            }
            let entries = payload["setVariableResult"].as_array()?;
            if entries.len() != request.set_variable_data.len() {
                return None;
            }
            let mut identities = request
                .set_variable_data
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    (
                        identity_key(
                            &entry.component,
                            &entry.variable,
                            entry.attribute_type.unwrap_or_default(),
                        ),
                        index,
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let mut results = vec![None; entries.len()];
            for entry in entries {
                let component = component(&entry["component"])?;
                let variable = variable(&entry["variable"])?;
                if !crate::command_registry::device_model201::contained(resource, &component) {
                    return None;
                }
                let attribute = entry
                    .get("attributeType")
                    .map(DeviceAttributeType201::deserialize)
                    .transpose()
                    .ok()?
                    .unwrap_or_default();
                let index = identities.remove(&identity_key(&component, &variable, attribute))?;
                let original = &request.set_variable_data[index];
                results[index] = Some(SetVariableResult201 {
                    component: original.component.clone(),
                    variable: original.variable.clone(),
                    attribute_type: original.attribute_type.unwrap_or_default(),
                    status: SetVariableStatus201::deserialize(&entry["attributeStatus"]).ok()?,
                });
            }
            Some(ConfigurationResult201::SetVariables {
                variables: results.into_iter().collect::<Option<Vec<_>>>()?,
            })
        }
        Request::Network(request) => {
            if !registry::valid_schema(3, payload) {
                return None;
            }
            let status = SetNetworkProfileStatus201::deserialize(&payload["status"]).ok()?;
            Some(ConfigurationResult201::SetNetworkProfile {
                configuration_slot: request.configuration_slot,
                status,
                staged: status == SetNetworkProfileStatus201::Accepted,
            })
        }
    }
}
