use std::{collections::BTreeSet, fmt::Write as _};

use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response, sse::Event},
};
use serde::Serialize;
use uob_contracts::{CanonicalResource, ResourceRef};

use super::payload;

const MAXIMUM_SIGNAL_BYTES: usize = 16 * 1024;
const FALLBACK_SIGNAL: &str = "{\"error\":\"events.signal_encoding_failed\"}";

#[derive(Serialize)]
struct StreamSignal<'a> {
    kind: &'a str,
    error: &'a str,
    recovery: Option<SnapshotRecovery<'a>>,
}

#[derive(Serialize)]
struct SnapshotRecovery<'a> {
    action: &'a str,
    snapshot_url: String,
    resubscribe_url: String,
    omit_cursor: bool,
    resource: &'a ResourceRef,
    event_types: &'a BTreeSet<String>,
}

pub(super) struct RecoveryUrls {
    pub(super) snapshot: String,
    pub(super) resubscribe: String,
}

pub(super) fn gap_event(resource: &ResourceRef, event_types: &BTreeSet<String>) -> (Event, usize) {
    signal_event(
        "gap",
        &StreamSignal {
            kind: "durable_cursor_gap",
            error: "events.cursor_expired",
            recovery: Some(snapshot_recovery(resource, event_types)),
        },
    )
}

pub(super) fn cursor_expired_response(
    resource: &ResourceRef,
    event_types: &BTreeSet<String>,
) -> Response {
    let payload = encode_signal(&StreamSignal {
        kind: "durable_cursor_gap",
        error: "events.cursor_expired",
        recovery: Some(snapshot_recovery(resource, event_types)),
    });
    (
        StatusCode::GONE,
        [(header::CONTENT_TYPE, "application/json")],
        payload,
    )
        .into_response()
}

pub(super) fn error_event(code: &'static str) -> (Event, usize) {
    signal_event(
        "error",
        &StreamSignal {
            kind: "stream_error",
            error: code,
            recovery: None,
        },
    )
}

fn snapshot_recovery<'a>(
    resource: &'a ResourceRef,
    event_types: &'a BTreeSet<String>,
) -> SnapshotRecovery<'a> {
    let urls = urls(resource, event_types);
    SnapshotRecovery {
        action: "fetch_fresh_snapshot",
        snapshot_url: urls.snapshot,
        resubscribe_url: urls.resubscribe,
        omit_cursor: true,
        resource,
        event_types,
    }
}

fn signal_event(name: &'static str, signal: &StreamSignal<'_>) -> (Event, usize) {
    let payload = encode_signal(signal);
    let encoded_bytes = name.len() + payload.len() + 16;
    (Event::default().event(name).data(payload), encoded_bytes)
}

fn encode_signal(signal: &StreamSignal<'_>) -> String {
    payload::encode_json(signal, MAXIMUM_SIGNAL_BYTES)
        .unwrap_or_else(|_| FALLBACK_SIGNAL.to_owned())
}

pub(super) fn urls(resource: &ResourceRef, event_types: &BTreeSet<String>) -> RecoveryUrls {
    let mut snapshot = String::from("/api/v1/stations/");
    append_component(&mut snapshot, resource.station_id.as_str());

    let mut resubscribe = String::from("/api/v1/events");
    append_parameter(&mut resubscribe, "station_id", resource.station_id.as_str());
    match resource.resource.as_ref() {
        None => {}
        Some(CanonicalResource::Connector { connector_id }) => {
            append_parameter(&mut resubscribe, "connector_id", connector_id.as_str());
        }
        Some(CanonicalResource::Evse {
            evse_id,
            connector_id,
        }) => {
            append_parameter(&mut resubscribe, "evse_id", evse_id.as_str());
            if let Some(connector_id) = connector_id {
                append_parameter(&mut resubscribe, "connector_id", connector_id.as_str());
            }
        }
    }
    if !event_types.is_empty() {
        let event_types = event_types.iter().cloned().collect::<Vec<_>>().join(",");
        append_parameter(&mut resubscribe, "types", &event_types);
    }
    RecoveryUrls {
        snapshot,
        resubscribe,
    }
}

fn append_parameter(url: &mut String, name: &str, value: &str) {
    url.push(if url.contains('?') { '&' } else { '?' });
    url.push_str(name);
    url.push('=');
    append_component(url, value);
}

fn append_component(output: &mut String, value: &str) {
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            write!(output, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use uob_contracts::{
        BridgeId, CanonicalConnectorId, CanonicalEvseId, CanonicalResource, ResourceRef, StationId,
    };

    use super::urls;

    #[test]
    fn recovery_urls_preserve_and_encode_every_selector() {
        let resource = ResourceRef {
            bridge_id: BridgeId::new("bridge-api").unwrap(),
            station_id: StationId::new("station / one").unwrap(),
            resource: Some(CanonicalResource::Evse {
                evse_id: CanonicalEvseId::new("evse & one").unwrap(),
                connector_id: Some(CanonicalConnectorId::new("connector/1").unwrap()),
            }),
            native_protocol_reference: None,
        };
        let urls = urls(
            &resource,
            &BTreeSet::from(["meter & changed.v1".to_owned(), "status.v1".to_owned()]),
        );
        assert_eq!(urls.snapshot, "/api/v1/stations/station%20%2F%20one");
        assert_eq!(
            urls.resubscribe,
            "/api/v1/events?station_id=station%20%2F%20one&evse_id=evse%20%26%20one&connector_id=connector%2F1&types=meter%20%26%20changed.v1%2Cstatus.v1"
        );
    }
}
