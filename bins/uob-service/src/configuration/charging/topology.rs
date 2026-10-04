use super::{ConfigurationLoadError, ResourceConfiguration, valid_resource_name};
use std::collections::{BTreeMap, BTreeSet};
use uob_contracts::{
    BridgeId, CanonicalConnectorId, CanonicalEvseId, CanonicalResource, NativeProtocolReference,
    ProtocolEdition, ResourceRef, StationId,
};
pub(super) fn validate_resources(
    entries: Vec<ResourceConfiguration>,
    protocol: ProtocolEdition,
    bridge_id: &BridgeId,
    station_id: &StationId,
) -> Result<Vec<ResourceRef>, ConfigurationLoadError> {
    let fail = ConfigurationLoadError::InvalidCharging;
    let mut native_addresses = BTreeSet::new();
    let mut native_evse_map = BTreeMap::new();
    let mut canonical_evse_map = BTreeMap::new();
    let mut canonical_addresses = BTreeSet::new();
    let mut resources = Vec::with_capacity(entries.len() + 1);
    resources.push(ResourceRef {
        bridge_id: bridge_id.clone(),
        station_id: station_id.clone(),
        resource: None,
        native_protocol_reference: None,
    });
    for resource in entries {
        let (canonical, native, canonical_key, native_key) = match protocol {
            ProtocolEdition::Ocpp16j => {
                if resource.evse.is_some() || resource.native_evse.is_some() {
                    return Err(fail);
                }
                let connector = CanonicalConnectorId::new(valid_resource_name(
                    resource.connector.ok_or(fail)?,
                )?)
                .map_err(|_| fail)?;
                let number = resource.native_connector.filter(|id| *id > 0).ok_or(fail)?;
                let canonical_key = (None, Some(connector.as_str().to_owned()));
                (
                    CanonicalResource::Connector {
                        connector_id: connector,
                    },
                    NativeProtocolReference::Ocpp16 {
                        connector_id: number,
                    },
                    canonical_key,
                    (number, None),
                )
            }
            ProtocolEdition::Ocpp201 => {
                let evse = CanonicalEvseId::new(valid_resource_name(resource.evse.ok_or(fail)?)?)
                    .map_err(|_| fail)?;
                let native_evse = resource.native_evse.filter(|id| *id > 0).ok_or(fail)?;
                if native_evse_map
                    .insert(native_evse, evse.as_str().to_owned())
                    .is_some_and(|previous| previous != evse.as_str())
                    || canonical_evse_map
                        .insert(evse.as_str().to_owned(), native_evse)
                        .is_some_and(|previous| previous != native_evse)
                {
                    return Err(fail);
                }
                let connector = resource
                    .connector
                    .map(valid_resource_name)
                    .transpose()?
                    .map(CanonicalConnectorId::new)
                    .transpose()
                    .map_err(|_| fail)?;
                let native_connector = resource.native_connector;
                if connector.is_some() != native_connector.is_some() || native_connector == Some(0)
                {
                    return Err(fail);
                }
                let canonical_key = (
                    Some(evse.as_str().to_owned()),
                    connector.as_ref().map(|c| c.as_str().to_owned()),
                );
                (
                    CanonicalResource::Evse {
                        evse_id: evse,
                        connector_id: connector,
                    },
                    NativeProtocolReference::Ocpp201 {
                        evse_id: native_evse,
                        connector_id: native_connector,
                    },
                    canonical_key,
                    (native_evse, native_connector),
                )
            }
        };
        if !native_addresses.insert(native_key) || !canonical_addresses.insert(canonical_key) {
            return Err(fail);
        }
        resources.push(ResourceRef {
            bridge_id: bridge_id.clone(),
            station_id: station_id.clone(),
            resource: Some(canonical),
            native_protocol_reference: Some(native),
        });
    }
    Ok(resources)
}
