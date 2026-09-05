use super::request::Selection;
use crate::error::IntegrationErrorCode as Error;
use std::fmt::Write as _;
use uob_contracts::CanonicalResource;

pub(super) fn recovery(selection: &Selection) -> serde_json::Value {
    let resource = &selection.resource;
    let mut snapshot = String::from("/bridge/v1/stations/");
    component(&mut snapshot, resource.station_id.as_str());
    parameter(&mut snapshot, "bridge_id", resource.bridge_id.as_str());
    let mut subscribe = String::from("/bridge/v1/events");
    parameter(&mut subscribe, "bridge_id", resource.bridge_id.as_str());
    parameter(&mut subscribe, "station_id", resource.station_id.as_str());
    match &resource.resource {
        None => {}
        Some(CanonicalResource::Connector { connector_id }) => {
            parameter(&mut subscribe, "connector_id", connector_id.as_str());
        }
        Some(CanonicalResource::Evse {
            evse_id,
            connector_id,
        }) => {
            parameter(&mut subscribe, "evse_id", evse_id.as_str());
            if let Some(connector_id) = connector_id {
                parameter(&mut subscribe, "connector_id", connector_id.as_str());
            }
        }
    }
    if !selection.types.is_empty() {
        parameter(
            &mut subscribe,
            "types",
            &selection
                .types
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    serde_json::json!({
        "error": Error::CursorExpired.as_str(), "kind": "durable_cursor_gap",
        "recovery": { "action": "fetch_fresh_snapshot", "snapshot_url": snapshot, "resubscribe_url": subscribe, "resource": resource, "types": selection.types, "omit_cursor": true }
    })
}

fn parameter(url: &mut String, name: &str, value: &str) {
    url.push(if url.contains('?') { '&' } else { '?' });
    url.push_str(name);
    url.push('=');
    component(url, value);
}

fn component(output: &mut String, value: &str) {
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            write!(output, "%{byte:02X}").expect("writing a String");
        }
    }
}
