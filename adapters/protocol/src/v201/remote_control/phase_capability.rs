//! Positive phase-switching authority is connection-owned, never snapshot-restored.
use std::collections::BTreeMap;
use uob_contracts::{
    DeviceAttributeType201, DeviceComponent201, DeviceModelQuery201, DeviceVariable201,
    DeviceVariableResult201, DeviceVariableStatus201,
};

pub(super) fn identity(
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
) -> Option<i32> {
    if component.name != "SmartChargingCtrlr"
        || component.instance.is_some()
        || variable.name != "ACPhaseSwitchingSupported"
        || variable.instance.is_some()
    {
        return None;
    }
    let evse = component.evse.as_ref()?;
    (evse.id > 0 && evse.connector_id.is_none()).then_some(evse.id)
}

#[derive(Default)]
pub(super) struct PhaseCapabilities {
    next: u64,
    active: bool,
    entries: BTreeMap<i32, (u64, bool)>,
}
impl PhaseCapabilities {
    pub fn connected(snapshot: &uob_contracts::StationSnapshot) -> Self {
        let entries = snapshot
            .resources
            .iter()
            .filter_map(|entry| crate::command_registry::charging_profile201::evse(&entry.resource))
            .filter(|evse| *evse > 0)
            .map(|evse| (evse, (0, false)))
            .collect();
        Self {
            active: true,
            entries,
            ..Self::default()
        }
    }
    /// Issuance revokes existing proof immediately. Only the latest matching request can grant it.
    pub fn begin(&mut self, query: &DeviceModelQuery201) -> u64 {
        self.next = self.next.saturating_add(1);
        let sequence = self.next;
        if let DeviceModelQuery201::GetVariables { entries } = query {
            for entry in entries {
                if entry.attribute_type.unwrap_or_default() == DeviceAttributeType201::Actual
                    && let Some(evse) = identity(&entry.component, &entry.variable)
                    && self.entries.contains_key(&evse)
                {
                    self.entries.insert(evse, (sequence, false));
                }
            }
        }
        sequence
    }
    pub fn finish(
        &mut self,
        sequence: u64,
        query: &DeviceModelQuery201,
        results: Option<&[DeviceVariableResult201]>,
    ) {
        if !self.active {
            return;
        }
        let DeviceModelQuery201::GetVariables { entries } = query else {
            return;
        };
        for entry in entries {
            if entry.attribute_type.unwrap_or_default() != DeviceAttributeType201::Actual {
                continue;
            }
            let Some(evse) = identity(&entry.component, &entry.variable) else {
                continue;
            };
            let Some((current, proven)) = self.entries.get_mut(&evse) else {
                continue;
            };
            *proven = *current == sequence
                && results.is_some_and(|results| {
                    results.iter().any(|result| {
                        result.component == entry.component
                            && result.variable == entry.variable
                            && result.attribute_type == DeviceAttributeType201::Actual
                            && result.status == DeviceVariableStatus201::Accepted
                            && !result.value.redacted
                            && result.value.value.as_deref() == Some("true")
                    })
                });
        }
    }
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn supports(&self, evse: i32) -> bool {
        self.active && self.entries.get(&evse).is_some_and(|(_, proven)| *proven)
    }
    pub fn detach(&mut self) {
        self.active = false;
        self.entries.clear();
    }
}
