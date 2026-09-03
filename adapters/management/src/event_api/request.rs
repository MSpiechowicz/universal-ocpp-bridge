use std::collections::BTreeSet;

use axum::http::{HeaderMap, header};
use serde::Deserialize;
use uob_application::{Application, RetainedEventCursor};
use uob_contracts::{
    BridgeId, CanonicalConnectorId, CanonicalEvseId, CanonicalResource, ResourceRef, StationId,
};

use super::{AuthenticatedEventAccess, unsafe_text};

const MAX_AUTHORIZATION_BYTES: usize = 8 * 1024;
const MAX_FILTER_ID_BYTES: usize = 256;
const MAX_EVENT_TYPE_BYTES: usize = 128;
const MAX_EVENT_TYPES: usize = 8;
const MAX_EVENT_TYPE_FILTER_BYTES: usize =
    MAX_EVENT_TYPES * MAX_EVENT_TYPE_BYTES + MAX_EVENT_TYPES - 1;
const MAX_SSE_ID_BYTES: usize = 512;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EventQuery {
    station_id: Option<String>,
    evse_id: Option<String>,
    connector_id: Option<String>,
    types: Option<String>,
    after: Option<String>,
}

pub(super) struct ValidatedEventQuery {
    pub(super) resource: ResourceRef,
    pub(super) event_types: BTreeSet<String>,
    pub(super) after: Option<RetainedEventCursor>,
}

pub(super) fn validate_query(
    query: EventQuery,
    headers: &HeaderMap,
    application: &Application,
    access: &AuthenticatedEventAccess,
) -> Result<ValidatedEventQuery, &'static str> {
    let resource = match query.station_id {
        Some(station_id) => explicit_resource(
            application.identity().bridge_id.clone(),
            station_id,
            query.evse_id,
            query.connector_id,
        )?,
        None if query.evse_id.is_none() && query.connector_id.is_none() => {
            let mut resource = access.default_resource.clone();
            resource.native_protocol_reference = None;
            bounded_resource(&resource)?;
            resource
        }
        None => return Err("events.invalid_resource_filter"),
    };
    let event_types = event_types(query.types)?;
    let header_cursor = header_cursor(headers)?;
    let query_cursor = query.after.as_deref();
    if let (Some(header), Some(parameter)) = (header_cursor, query_cursor)
        && header != parameter
    {
        return Err("events.cursor_conflict");
    }
    let after = header_cursor
        .or(query_cursor)
        .map(parse_cursor)
        .transpose()?;
    Ok(ValidatedEventQuery {
        resource,
        event_types,
        after,
    })
}

pub(super) fn bearer_token(headers: &HeaderMap) -> Result<&str, ()> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next().ok_or(())?;
    if values.next().is_some() || value.as_bytes().len() > MAX_AUTHORIZATION_BYTES {
        return Err(());
    }
    let value = value.to_str().map_err(|_| ())?;
    let (scheme, token) = value.split_once(' ').ok_or(())?;
    if !scheme.eq_ignore_ascii_case("bearer")
        || token.is_empty()
        || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(());
    }
    Ok(token)
}

fn explicit_resource(
    bridge_id: BridgeId,
    station_id: String,
    evse_id: Option<String>,
    connector_id: Option<String>,
) -> Result<ResourceRef, &'static str> {
    bounded_filter(&station_id)?;
    let station_id = StationId::new(station_id).map_err(|_| "events.invalid_resource_filter")?;
    let resource = match (evse_id, connector_id) {
        (None, None) => None,
        (None, Some(connector_id)) => {
            bounded_filter(&connector_id)?;
            Some(CanonicalResource::Connector {
                connector_id: CanonicalConnectorId::new(connector_id)
                    .map_err(|_| "events.invalid_resource_filter")?,
            })
        }
        (Some(evse_id), connector_id) => {
            bounded_filter(&evse_id)?;
            let connector_id = connector_id
                .map(|value| {
                    bounded_filter(&value)?;
                    CanonicalConnectorId::new(value).map_err(|_| "events.invalid_resource_filter")
                })
                .transpose()?;
            Some(CanonicalResource::Evse {
                evse_id: CanonicalEvseId::new(evse_id)
                    .map_err(|_| "events.invalid_resource_filter")?,
                connector_id,
            })
        }
    };
    let resource = ResourceRef {
        bridge_id,
        station_id,
        resource,
        native_protocol_reference: None,
    };
    bounded_resource(&resource)?;
    Ok(resource)
}

fn event_types(value: Option<String>) -> Result<BTreeSet<String>, &'static str> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    if value.len() > MAX_EVENT_TYPE_FILTER_BYTES {
        return Err("events.invalid_type_filter");
    }
    let values: Vec<_> = value.split(',').collect();
    if values.is_empty()
        || values.len() > MAX_EVENT_TYPES
        || values.iter().any(|value| {
            value.is_empty() || value.len() > MAX_EVENT_TYPE_BYTES || unsafe_text(value)
        })
    {
        return Err("events.invalid_type_filter");
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

fn bounded_filter(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > MAX_FILTER_ID_BYTES || unsafe_text(value) {
        Err("events.invalid_resource_filter")
    } else {
        Ok(())
    }
}

fn bounded_resource(resource: &ResourceRef) -> Result<(), &'static str> {
    bounded_filter(resource.bridge_id.as_str())?;
    bounded_filter(resource.station_id.as_str())?;
    match resource.resource.as_ref() {
        None => Ok(()),
        Some(CanonicalResource::Connector { connector_id }) => {
            bounded_filter(connector_id.as_str())
        }
        Some(CanonicalResource::Evse {
            evse_id,
            connector_id,
        }) => {
            bounded_filter(evse_id.as_str())?;
            connector_id
                .as_ref()
                .map_or(Ok(()), |value| bounded_filter(value.as_str()))
        }
    }
}

fn parse_cursor(value: &str) -> Result<RetainedEventCursor, &'static str> {
    if value.len() > MAX_SSE_ID_BYTES || unsafe_text(value) {
        return Err("events.invalid_cursor");
    }
    RetainedEventCursor::new(value.to_owned()).map_err(|_| "events.invalid_cursor")
}

fn header_cursor(headers: &HeaderMap) -> Result<Option<&str>, &'static str> {
    let mut values = headers.get_all("last-event-id").iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err("events.invalid_cursor");
    }
    let value = value.to_str().map_err(|_| "events.invalid_cursor")?;
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::{bearer_token, event_types, parse_cursor};

    #[test]
    fn cursor_and_type_filters_reject_control_or_unbounded_input() {
        assert_eq!(
            parse_cursor("uob:event:1\nid:forged").unwrap_err(),
            "events.invalid_cursor"
        );
        let too_many = (0..9)
            .map(|value| format!("event.{value}.v1"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            event_types(Some(too_many)).unwrap_err(),
            "events.invalid_type_filter"
        );
        assert_eq!(
            event_types(Some(["event.v1"; 9].join(","))).unwrap_err(),
            "events.invalid_type_filter"
        );
    }

    #[test]
    fn bearer_syntax_is_strict_and_bounded() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token"),
        );
        assert_eq!(bearer_token(&headers), Ok("token"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        assert_eq!(bearer_token(&headers), Err(()));
    }
}
