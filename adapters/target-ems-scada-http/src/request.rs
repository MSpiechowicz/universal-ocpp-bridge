use serde::Deserialize;
use uob_application::{AccessPermission, AccessResourceScope, PageLimit, SnapshotCursor};
use uob_contracts::{
    BridgeId, CanonicalConnectorId, CanonicalEvseId, CanonicalResource, ResourceRef, StationId,
};

use crate::{configuration::IntegrationPrincipal, error::IntegrationErrorCode};

/// Page size served when a caller states none.
pub(crate) const DEFAULT_PAGE_SIZE: u16 = 25;

/// Largest page the application's own bounded-read type accepts.
///
/// The value is read back from [`PageLimit`] rather than restated, so the advertised bound cannot
/// drift away from the limit that is actually enforced.
pub(crate) fn maximum_page_size() -> u16 {
    match PageLimit::new(u16::MAX) {
        Ok(limit) => limit.get(),
        Err(uob_application::PageLimitError::TooLarge { maximum, .. }) => maximum,
        Err(uob_application::PageLimitError::Zero) => 1,
    }
}

/// Bounded pagination accepted by every integration list.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageParameters {
    /// Opaque cursor returned by the previous page.
    pub(crate) after: Option<String>,
    /// Requested page size, bounded by the application's own maximum.
    pub(crate) limit: Option<u16>,
}

impl PageParameters {
    /// Validates the requested page size against the application-owned maximum.
    ///
    /// An oversized or zero request is refused rather than silently clamped, so a caller cannot
    /// discover the bound by watching truncated pages.
    pub(crate) fn page_limit(&self) -> Result<PageLimit, IntegrationErrorCode> {
        PageLimit::new(self.limit.unwrap_or(DEFAULT_PAGE_SIZE))
            .map_err(|_| IntegrationErrorCode::InvalidRequest)
    }
}

/// Canonical resource selection shared by the station and point resources.
// The field names are the query parameter names an integration client actually sends, so they
// mirror the canonical identifiers rather than being shortened for the struct's sake.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResourceParameters {
    /// Bridge installation, required only when the credential spans several.
    pub(crate) bridge_id: Option<String>,
    /// Station owning the addressed resource.
    pub(crate) station_id: Option<String>,
    /// EVSE below the station, for the OCPP 2.0.1 resource model.
    pub(crate) evse_id: Option<String>,
    /// Connector below the station or EVSE, for either resource model.
    pub(crate) connector_id: Option<String>,
}

impl ResourceParameters {
    /// Builds the canonical resource reference this request addresses.
    ///
    /// The station is required; an EVSE, a connector, or an EVSE-scoped connector narrows it
    /// without losing which of the two OCPP resource models the caller named.
    pub(crate) fn resource(
        &self,
        principal: &IntegrationPrincipal,
    ) -> Result<ResourceRef, IntegrationErrorCode> {
        let Some(station_id) = self.station_id.as_ref() else {
            return Err(IntegrationErrorCode::InvalidRequest);
        };
        self.resource_for_station(principal, station_id)
    }

    /// Builds the reference for an explicitly named station.
    pub(crate) fn resource_for_station(
        &self,
        principal: &IntegrationPrincipal,
        station_id: &str,
    ) -> Result<ResourceRef, IntegrationErrorCode> {
        let station_id =
            StationId::new(station_id).map_err(|_| IntegrationErrorCode::InvalidRequest)?;
        Ok(ResourceRef {
            bridge_id: self.bridge_id(principal)?,
            station_id,
            resource: self.canonical_resource()?,
            native_protocol_reference: None,
        })
    }

    /// Resolves the bridge installation the request addresses.
    ///
    /// A credential scoped to exactly one bridge needs no parameter. A credential holding scopes
    /// in several bridges must name one, so a station identifier can never resolve to a different
    /// installation than the caller meant.
    fn bridge_id(
        &self,
        principal: &IntegrationPrincipal,
    ) -> Result<BridgeId, IntegrationErrorCode> {
        if let Some(bridge_id) = &self.bridge_id {
            return BridgeId::new(bridge_id.as_str())
                .map_err(|_| IntegrationErrorCode::InvalidRequest);
        }
        let mut granted = granted_bridges(principal);
        granted.dedup();
        match granted.as_slice() {
            [bridge_id] => Ok((*bridge_id).clone()),
            [] => Err(IntegrationErrorCode::PermissionDenied),
            _ => Err(IntegrationErrorCode::BridgeRequired),
        }
    }

    /// Builds the optional EVSE/connector part of the canonical reference.
    fn canonical_resource(&self) -> Result<Option<CanonicalResource>, IntegrationErrorCode> {
        let connector_id = self
            .connector_id
            .as_ref()
            .map(|value| CanonicalConnectorId::new(value.as_str()))
            .transpose()
            .map_err(|_| IntegrationErrorCode::InvalidRequest)?;
        let evse_id = self
            .evse_id
            .as_ref()
            .map(|value| CanonicalEvseId::new(value.as_str()))
            .transpose()
            .map_err(|_| IntegrationErrorCode::InvalidRequest)?;
        Ok(match (evse_id, connector_id) {
            (None, None) => None,
            (None, Some(connector_id)) => Some(CanonicalResource::Connector { connector_id }),
            (Some(evse_id), connector_id) => Some(CanonicalResource::Evse {
                evse_id,
                connector_id,
            }),
        })
    }

    /// Returns whether this request narrows a station to one EVSE or connector.
    pub(crate) const fn narrows_resource(&self) -> bool {
        self.evse_id.is_some() || self.connector_id.is_some()
    }
}

/// Returns the bridge installations named by a credential's own resource scopes.
fn granted_bridges(principal: &IntegrationPrincipal) -> Vec<&BridgeId> {
    let mut granted: Vec<&BridgeId> = principal
        .resource_scopes()
        .iter()
        .map(|scope| match scope {
            AccessResourceScope::Bridge(bridge_id)
            | AccessResourceScope::Station { bridge_id, .. } => bridge_id,
            AccessResourceScope::Resource(resource) => &resource.bridge_id,
        })
        .collect();
    granted.sort();
    granted
}

/// Confirms the credential itself holds the reader role before any canonical read is attempted.
///
/// The host's scoped port already restricts the configured target instance. This check is the
/// credential's own boundary, so one listener can serve several principals with different scopes.
pub(crate) fn require_reader(
    principal: Option<&IntegrationPrincipal>,
) -> Result<&IntegrationPrincipal, IntegrationErrorCode> {
    let principal = principal.ok_or(IntegrationErrorCode::PermissionDenied)?;
    if principal.permissions().contains(&AccessPermission::Read) {
        Ok(principal)
    } else {
        Err(IntegrationErrorCode::PermissionDenied)
    }
}

/// Returns whether the calling credential may read one canonical resource.
pub(crate) fn permits_read(principal: &IntegrationPrincipal, resource: &ResourceRef) -> bool {
    principal.grant().permits(AccessPermission::Read, resource)
}

/// Parses one opaque station-page cursor.
///
/// A position belonging to another bounded list is refused here rather than passed down: the
/// application's cursor type accepts any opaque text, so only the listener can tell a caller that
/// it handed back a point-page position instead of a station-page one.
pub(crate) fn snapshot_cursor(
    after: Option<&String>,
) -> Result<Option<SnapshotCursor>, IntegrationErrorCode> {
    let Some(value) = after else {
        return Ok(None);
    };
    if value.starts_with(crate::points::POINT_CURSOR_PREFIX) {
        return Err(IntegrationErrorCode::InvalidRequest);
    }
    SnapshotCursor::new(value.as_str())
        .map(Some)
        .map_err(|_| IntegrationErrorCode::InvalidRequest)
}

/// Returns whether any of the credential's scopes could match a resource below one station.
///
/// A point request names a station, but a credential may be scoped to a single EVSE below it. The
/// station itself is therefore not the unit of the check; the question is only whether this
/// credential holds anything at all inside that station.
pub(crate) fn intersects_station(principal: &IntegrationPrincipal, station: &ResourceRef) -> bool {
    principal.resource_scopes().iter().any(|scope| match scope {
        AccessResourceScope::Bridge(bridge_id) => *bridge_id == station.bridge_id,
        AccessResourceScope::Station {
            bridge_id,
            station_id,
        } => *bridge_id == station.bridge_id && *station_id == station.station_id,
        AccessResourceScope::Resource(resource) => {
            resource.bridge_id == station.bridge_id && resource.station_id == station.station_id
        }
    })
}
