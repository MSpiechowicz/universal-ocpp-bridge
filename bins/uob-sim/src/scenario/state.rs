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
    pub last_sequence: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StationState {
    pub station_id: String,
    pub version: OcppVersion,
    pub connected: bool,
    pub registered: bool,
    resources: HashMap<StationResource, ResourceState>,
    authorized_tags: HashSet<String>,
    target_online: bool,
    command_requests: HashMap<String, CommandState>,
    command_deliveries: HashSet<String>,
    physical_effects: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandState {
    Expired,
    Rejected,
    TransmissionUncertain,
    Confirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandAdmission {
    New,
    Duplicate,
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
            target_online: true,
            command_requests: HashMap::new(),
            command_deliveries: HashSet::new(),
            physical_effects: 0,
        }
    }

    pub fn authorize(&mut self, id_tag: impl Into<String>) {
        self.authorized_tags.insert(id_tag.into());
    }

    #[must_use]
    pub fn is_authorized(&self, id_tag: &str) -> bool {
        self.authorized_tags.contains(id_tag)
    }

    pub(crate) fn set_target_online(&mut self, online: bool) {
        self.target_online = online;
    }

    pub(crate) const fn target_online(&self) -> bool {
        self.target_online
    }

    pub(crate) const fn physical_effects(&self) -> u64 {
        self.physical_effects
    }

    pub(crate) fn record_local_effect(&mut self) {
        self.physical_effects = self.physical_effects.saturating_add(1);
    }

    pub(crate) fn admit_command(
        &mut self,
        request_id: &str,
        delivery_id: &str,
        expired: bool,
    ) -> CommandAdmission {
        if self.command_requests.contains_key(request_id)
            || self.command_deliveries.contains(delivery_id)
        {
            return CommandAdmission::Duplicate;
        }
        self.command_deliveries.insert(delivery_id.to_owned());
        if expired {
            self.command_requests
                .insert(request_id.to_owned(), CommandState::Expired);
        }
        CommandAdmission::New
    }

    pub(crate) fn complete_command(
        &mut self,
        request_id: &str,
        accepted: bool,
        response_lost: bool,
    ) {
        let state = if !accepted {
            CommandState::Rejected
        } else if response_lost {
            self.physical_effects = self.physical_effects.saturating_add(1);
            CommandState::TransmissionUncertain
        } else {
            self.physical_effects = self.physical_effects.saturating_add(1);
            CommandState::Confirmed
        };
        self.command_requests.insert(request_id.to_owned(), state);
    }

    pub(crate) fn reconcile_command(&mut self, request_id: &str) -> bool {
        let Some(state) = self.command_requests.get_mut(request_id) else {
            return false;
        };
        if *state != CommandState::TransmissionUncertain {
            return false;
        }
        *state = CommandState::Confirmed;
        true
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
        let state = self
            .resources
            .get_mut(&resource)
            .ok_or(StationStateError::UnknownResource)?;
        let transaction_id = state
            .transaction_id
            .take()
            .ok_or(StationStateError::NoActiveTransaction)?;
        state.last_sequence = None;
        Ok(transaction_id)
    }

    /// Records the latest accepted transaction-event sequence for a resource.
    ///
    /// # Errors
    ///
    /// Rejects identities from the wrong protocol model, unknown resources, and idle resources.
    pub fn record_sequence(
        &mut self,
        resource: StationResource,
        sequence: i64,
    ) -> Result<(), StationStateError> {
        self.validate_resource_kind(resource)?;
        let state = self
            .resources
            .get_mut(&resource)
            .ok_or(StationStateError::UnknownResource)?;
        if state.transaction_id.is_none() {
            return Err(StationStateError::NoActiveTransaction);
        }
        state.last_sequence = Some(sequence);
        Ok(())
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
            last_sequence: None,
        }
    }
}
