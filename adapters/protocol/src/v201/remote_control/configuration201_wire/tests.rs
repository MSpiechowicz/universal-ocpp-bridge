use super::super::configuration201_values::{ProtectedVariableText201, ProtectedVariableValue201};
use super::{DeferredConfigurationCall201, LearnedLimits, LocalConfigurationValues201, Request};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uob_application::CommandClock;
use uob_contracts::{
    DeviceComponent201, DeviceVariable201, ResourceRef, SetVariableReference201,
    SetVariablesReference201, UtcTimestamp,
};
struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        serde_json::from_value(json!("2026-09-01T00:00:00Z")).unwrap()
    }
}
fn fixture() -> DeferredConfigurationCall201 {
    let resource: ResourceRef =
        serde_json::from_value(json!({"bridge_id":"bridge","station_id":"station"})).unwrap();
    let entries: Vec<_> = ["First", "Second"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| SetVariableReference201 {
            component: DeviceComponent201 {
                name: "Vendor".into(),
                instance: None,
                evse: None,
            },
            variable: DeviceVariable201 {
                name: name.into(),
                instance: None,
            },
            attribute_type: None,
            value_reference: format!("cfg201:{index:064x}"),
        })
        .collect();
    let values = entries
        .iter()
        .cloned()
        .map(|entry| ProtectedVariableValue201 {
            resource: resource.clone(),
            entry,
            value: ProtectedVariableText201::new("SECRET-ESCAPED\"\\\n😀".repeat(50)).unwrap(),
            expires_at: serde_json::from_value(json!("2026-09-02T00:00:00Z")).unwrap(),
        })
        .collect();
    DeferredConfigurationCall201 {
        provider: Arc::new(LocalConfigurationValues201::new(values, vec![]).unwrap()),
        clock: Arc::new(Clock),
        resource,
        request: Arc::new(Request::Variables(SetVariablesReference201 {
            set_variable_data: entries,
        })),
        limits: Arc::new(Mutex::new(LearnedLimits::default())),
        active: Arc::new(Mutex::new(true)),
        expires_at: serde_json::from_value(json!("2026-09-02T00:00:00Z")).unwrap(),
        _metadata: Arc::new(
            uob_application::RuntimeResourceBudget::new(
                uob_application::RuntimeResourceLimits::default(),
            )
            .unwrap()
            .try_reserve(uob_application::WorkClass::PendingRequest, 4096)
            .unwrap(),
        ),
    }
}
#[test]
fn retained_references_cannot_keep_old_limits_or_send_a_partial_resolved_batch() {
    let deferred = fixture();
    assert!(deferred.wire_size("queued").is_none());
    {
        let Ok(mut limits) = deferred.limits.lock() else {
            panic!("poisoned test policy");
        };
        limits.values[2] = [Some(2), Some(16384)];
    }
    let expected = json!([2,"queued","SetVariables",{"setVariableData":[
        {"component":{"name":"Vendor"},"variable":{"name":"First"},"attributeValue":"SECRET-ESCAPED\"\\\n😀".repeat(50)},
        {"component":{"name":"Vendor"},"variable":{"name":"Second"},"attributeValue":"SECRET-ESCAPED\"\\\n😀".repeat(50)}
    ]}]);
    let native_size = expected.to_string().len();
    assert_eq!(deferred.wire_size("queued"), Some(native_size));
    let initial = deferred
        .resolve("queued", |entries, size| {
            let mut bytes = Vec::with_capacity(size);
            deferred.write(entries, "queued", &mut bytes)?;
            Some(serde_json::from_slice::<Value>(&bytes).unwrap())
        })
        .unwrap();
    assert_eq!(initial, expected);
    {
        let Ok(mut limits) = deferred.limits.lock() else {
            panic!("poisoned test policy");
        };
        limits.values[2][1] = Some(native_size - 1);
    }
    assert!(deferred.resolve("queued", |_, _| Some(())).is_none());
    {
        let Ok(mut limits) = deferred.limits.lock() else {
            panic!("poisoned test policy");
        };
        limits.values[2][1] = Some(native_size);
    }
    assert!(deferred.resolve("queued", |_, _| Some(())).is_some());
    deferred
        .provider
        .revoke(&format!("cfg201:{:064x}", 1))
        .unwrap();
    let mut callback_called = false;
    assert!(
        deferred
            .resolve("queued", |_, _| {
                callback_called = true;
                Some(())
            })
            .is_none()
    );
    assert!(
        !callback_called,
        "atomic all-reference validation must precede any native materialization"
    );
}
