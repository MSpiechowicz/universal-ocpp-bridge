use std::collections::{HashMap, HashSet};

use super::{ConfiguredOcppVersion, StationDefinition};
use crate::OcppVersion;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StationResource {
    Connector { connector_id: u16 },
    EvseConnector { evse_id: u16, connector_id: u16 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceState {
    pub transaction_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationState {
    pub station_id: String,
    pub version: OcppVersion,
    pub connected: bool,
    pub registered: bool,
    resources: HashMap<StationResource, ResourceState>,
    authorized_tags: HashSet<String>,
}

impl StationState {
    /// Builds simulator-owned state from a validated station definition.
    #[must_use]
    pub fn from_definition(definition: &StationDefinition) -> Self {
        let resources = match definition.ocpp_version {
            ConfiguredOcppVersion::V1_6 => definition
                .connector_ids()
                .into_iter()
                .map(|connector_id| StationResource::Connector { connector_id })
                .map(|resource| (resource, ResourceState::idle()))
                .collect(),
            ConfiguredOcppVersion::V2_0_1 => definition
                .evse_connectors()
                .into_iter()
                .map(|(evse_id, connector_id)| StationResource::EvseConnector {
                    evse_id,
                    connector_id,
                })
                .map(|resource| (resource, ResourceState::idle()))
                .collect(),
        };
        Self {
            station_id: definition.id.clone(),
            version: definition.ocpp_version.into(),
            connected: false,
            registered: false,
            resources,
            authorized_tags: HashSet::new(),
        }
    }

    pub fn authorize(&mut self, id_tag: impl Into<String>) {
        self.authorized_tags.insert(id_tag.into());
    }

    #[must_use]
    pub fn is_authorized(&self, id_tag: &str) -> bool {
        self.authorized_tags.contains(id_tag)
    }

    #[must_use]
    pub fn resource_for_transaction(&self, transaction_id: &str) -> Option<StationResource> {
        self.resources.iter().find_map(|(resource, state)| {
            (state.transaction_id.as_deref() == Some(transaction_id)).then_some(*resource)
        })
    }

    #[must_use]
    pub fn resource(&self, resource: StationResource) -> Option<&ResourceState> {
        self.resources.get(&resource)
    }

    /// Starts a simulator-local transaction on exactly one station resource.
    ///
    /// # Errors
    ///
    /// Rejects an unknown resource, an identity from the wrong OCPP model, or an occupied resource.
    pub fn start_transaction(
        &mut self,
        resource: StationResource,
        transaction_id: impl Into<String>,
    ) -> Result<(), StationStateError> {
        self.validate_resource_kind(resource)?;
        let state = self
            .resources
            .get_mut(&resource)
            .ok_or(StationStateError::UnknownResource)?;
        if state.transaction_id.is_some() {
            return Err(StationStateError::TransactionAlreadyActive);
        }
        state.transaction_id = Some(transaction_id.into());
        Ok(())
    }

    /// Stops and returns the simulator-local transaction for one station resource.
    ///
    /// # Errors
    ///
    /// Rejects an unknown resource, an identity from the wrong OCPP model, or an idle resource.
    pub fn stop_transaction(
        &mut self,
        resource: StationResource,
    ) -> Result<String, StationStateError> {
        self.validate_resource_kind(resource)?;
        self.resources
            .get_mut(&resource)
            .ok_or(StationStateError::UnknownResource)?
            .transaction_id
            .take()
            .ok_or(StationStateError::NoActiveTransaction)
    }

    fn validate_resource_kind(&self, resource: StationResource) -> Result<(), StationStateError> {
        if matches!(
            (self.version, resource),
            (OcppVersion::V1_6, StationResource::Connector { .. })
                | (OcppVersion::V2_0_1, StationResource::EvseConnector { .. })
        ) {
            Ok(())
        } else {
            Err(StationStateError::WrongProtocolIdentity)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StationStateError {
    WrongProtocolIdentity,
    UnknownResource,
    TransactionAlreadyActive,
    NoActiveTransaction,
}

impl ResourceState {
    const fn idle() -> Self {
        Self {
            transaction_id: None,
        }
    }
}
