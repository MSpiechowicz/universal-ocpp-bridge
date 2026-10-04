#[cfg(test)]
#[path = "local_authorization_limits_tests.rs"]
mod tests;
use caseless::Caseless;
use uob_contracts::{
    DeviceAttributeType201, DeviceComponent201, DeviceValue201, DeviceVariable201,
};

#[derive(Default)]
pub(crate) struct LocalListLimits201 {
    pub items: Option<usize>,
    pub bytes: Option<usize>,
    pub entries_actual: Option<usize>,
    pub capacity: Option<usize>,
    pub enabled: Option<bool>,
    pub available: Option<bool>,
    pub supports_expiry: Option<bool>,
    pub cache_enabled: Option<bool>,
}
fn named(text: &str, expected: &str) -> bool {
    text.chars().default_case_fold().eq(expected.chars())
}
pub(super) fn identity(
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
) -> Option<usize> {
    if component.instance.is_some() || component.evse.is_some() || variable.instance.is_some() {
        return None;
    }
    if named(&component.name, "authcachectrlr") && named(&variable.name, "enabled") {
        return Some(6);
    }
    if !named(&component.name, "localauthlistctrlr") {
        return None;
    }
    [
        "itemspermessage",
        "bytespermessage",
        "entries",
        "enabled",
        "available",
        "supportsexpirydatetime",
    ]
    .iter()
    .position(|name| named(&variable.name, name))
}
pub(super) fn safe_value(
    component: &DeviceComponent201,
    variable: &DeviceVariable201,
    value: &str,
) -> bool {
    match identity(component, variable) {
        Some(0..=2) => {
            !value.is_empty()
                && value.len() <= 10
                && value.bytes().all(|b| b.is_ascii_digit())
                && value.parse::<u32>().is_ok_and(|n| i32::try_from(n).is_ok())
        }
        Some(3..=6) => matches!(value, "true" | "false"),
        _ => false,
    }
}
impl LocalListLimits201 {
    pub fn allows(&self, count: usize, upsert_count: usize) -> bool {
        self.enabled != Some(false)
            && self.available != Some(false)
            && self.items.is_none_or(|max| count <= max)
            && self.capacity.is_none_or(|max| upsert_count <= max)
    }
    pub fn learn(
        &mut self,
        component: &DeviceComponent201,
        variable: &DeviceVariable201,
        attribute: DeviceAttributeType201,
        value: &DeviceValue201,
    ) {
        if attribute != DeviceAttributeType201::Actual {
            return;
        }
        let Some(value) = value.value.as_deref() else {
            return;
        };
        if !safe_value(component, variable, value) {
            return;
        }
        match identity(component, variable) {
            Some(0) => self.items = value.parse().ok(),
            Some(1) => self.bytes = value.parse().ok(),
            Some(2) => self.entries_actual = value.parse().ok(),
            Some(3) => self.enabled = Some(value == "true"),
            Some(4) => self.available = Some(value == "true"),
            Some(5) => self.supports_expiry = Some(value == "true"),
            Some(6) => self.cache_enabled = Some(value == "true"),
            _ => {}
        }
    }
}
