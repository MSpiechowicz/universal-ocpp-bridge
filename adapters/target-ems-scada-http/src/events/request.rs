use std::collections::BTreeSet;

use axum::http::HeaderMap;
use serde::Deserialize;
use uob_application::RetainedEventCursor;
use uob_contracts::ResourceRef;

use crate::{
    configuration::IntegrationPrincipal,
    error::IntegrationErrorCode as Error,
    request::{ResourceParameters, permits_read},
};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Parameters {
    bridge_id: Option<String>,
    station_id: Option<String>,
    evse_id: Option<String>,
    connector_id: Option<String>,
    types: Option<String>,
    after: Option<String>,
}

pub(super) struct Selection {
    pub resource: ResourceRef,
    pub types: BTreeSet<String>,
    pub after: Option<RetainedEventCursor>,
}

impl Parameters {
    pub(super) fn validate(
        self,
        headers: &HeaderMap,
        principal: &IntegrationPrincipal,
    ) -> Result<Selection, Error> {
        for value in [
            &self.bridge_id,
            &self.station_id,
            &self.evse_id,
            &self.connector_id,
        ]
        .into_iter()
        .flatten()
        {
            if value.len() > 256 || value.chars().any(char::is_control) {
                return Err(Error::InvalidRequest);
            }
        }
        let resource = ResourceParameters {
            bridge_id: self.bridge_id,
            station_id: self.station_id,
            evse_id: self.evse_id,
            connector_id: self.connector_id,
        }
        .resource(principal)?;
        // Replay recovery uses the station snapshot. Require its read scope up front so a
        // cursor gap can never direct this reader to a forbidden recovery surface.
        let mut station = resource.clone();
        station.resource = None;
        if !permits_read(principal, &resource) || !permits_read(principal, &station) {
            return Err(Error::PermissionDenied);
        }
        let mut types = BTreeSet::new();
        if let Some(value) = self.types {
            if value.len() > 1031 {
                return Err(Error::InvalidRequest);
            }
            for (index, part) in value.split(',').enumerate() {
                if index >= 8
                    || part.is_empty()
                    || part.len() > 128
                    || part.chars().any(char::is_control)
                {
                    return Err(Error::InvalidRequest);
                }
                types.insert(part.to_owned());
            }
        }
        let mut values = headers.get_all("last-event-id").iter();
        let header = values
            .next()
            .map(|v| v.to_str().map_err(|_| Error::InvalidRequest))
            .transpose()?
            .filter(|v| !v.is_empty());
        if values.next().is_some()
            || matches!((header, self.after.as_deref()), (Some(a), Some(b)) if a != b)
        {
            return Err(Error::InvalidRequest);
        }
        let after = header
            .or(self.after.as_deref())
            .map(|value| {
                if !safe_cursor(value) {
                    return Err(Error::InvalidRequest);
                }
                RetainedEventCursor::new(value).map_err(|_| Error::InvalidRequest)
            })
            .transpose()?;
        Ok(Selection {
            resource,
            types,
            after,
        })
    }
}

pub(super) fn safe_cursor(value: &str) -> bool {
    value.starts_with("uob:event:") && value.len() <= 512 && !value.chars().any(char::is_control)
}
