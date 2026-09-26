use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde_json::Value;
use subtle::ConstantTimeEq;
use uob_application::{
    AccessGrant, AccessPermission, AccessPolicy, AccessResourceScope, CommandAdmissionPort,
    ScopedCommandAdmissionPort,
};
use uob_contracts::{AuthenticatedCommandOrigin, Environment, PrincipalId, ResourceRef, StationId};
use uob_management_adapter::{
    ManagementCommandAuthenticator, ManagementCommandConfiguration, PrivilegedPayloadValidator,
    token_matches_environment,
};

use super::files::ReadGrant;

pub(super) struct ControlCredentials {
    environment: Environment,
    control: ReadGrant,
    privileged: Option<ReadGrant>,
    control_origin: AuthenticatedCommandOrigin,
    privileged_origin: AuthenticatedCommandOrigin,
    stations: BTreeSet<StationId>,
    start_refs: BTreeMap<StationId, String>,
}

impl ControlCredentials {
    pub(super) fn new(
        environment: Environment,
        read: &ReadGrant,
        control: ReadGrant,
        privileged: Option<ReadGrant>,
        roster: &[ResourceRef],
        start_refs: BTreeMap<StationId, String>,
    ) -> Result<Self, &'static str> {
        let valid = |grant: &ReadGrant| {
            std::str::from_utf8(grant.as_bytes())
                .is_ok_and(|token| token_matches_environment(token, environment))
        };
        if !valid(&control)
            || privileged.as_ref().is_some_and(|grant| !valid(grant))
            || bool::from(read.as_bytes().ct_eq(control.as_bytes()))
            || privileged.as_ref().is_some_and(|grant| {
                bool::from(grant.as_bytes().ct_eq(read.as_bytes()))
                    || bool::from(grant.as_bytes().ct_eq(control.as_bytes()))
            })
        {
            return Err("charging control credentials invalid");
        }
        Ok(Self {
            environment,
            control,
            privileged,
            control_origin: AuthenticatedCommandOrigin::Management {
                principal_id: PrincipalId::new("management-control")
                    .map_err(|_| "invalid control origin")?,
            },
            privileged_origin: AuthenticatedCommandOrigin::Management {
                principal_id: PrincipalId::new("management-privileged")
                    .map_err(|_| "invalid privileged origin")?,
            },
            stations: roster
                .iter()
                .map(|resource| resource.station_id.clone())
                .collect(),
            start_refs,
        })
    }

    pub(super) fn configuration(
        self: Arc<Self>,
        roster: &[ResourceRef],
        inner: Arc<dyn CommandAdmissionPort<Value>>,
    ) -> Result<ManagementCommandConfiguration, &'static str> {
        let scopes = roster
            .iter()
            .map(|resource| AccessResourceScope::Station {
                bridge_id: resource.bridge_id.clone(),
                station_id: resource.station_id.clone(),
            })
            .collect::<Vec<_>>();
        let mut grants = vec![
            AccessGrant::new(
                self.control_origin.clone(),
                vec![AccessPermission::Control],
                scopes.clone(),
            )
            .map_err(|_| "control scope invalid")?,
        ];
        if self.privileged.is_some() {
            grants.push(
                AccessGrant::new(
                    self.privileged_origin.clone(),
                    vec![AccessPermission::PrivilegedControl],
                    scopes,
                )
                .map_err(|_| "privileged scope invalid")?,
            );
        }
        let policy = AccessPolicy::new(grants).map_err(|_| "control policy invalid")?;
        Ok(ManagementCommandConfiguration {
            admission: Arc::new(ScopedCommandAdmissionPort::new(inner, policy)),
            authenticator: self,
            privileged_payloads: Arc::new(PinnedPayloads),
        })
    }
}

impl ManagementCommandAuthenticator for ControlCredentials {
    fn authenticate(&self, bearer_token: &str) -> Option<AuthenticatedCommandOrigin> {
        let valid_audience = token_matches_environment(bearer_token, self.environment);
        let control = bool::from(self.control.as_bytes().ct_eq(bearer_token.as_bytes()));
        let privileged = self
            .privileged
            .as_ref()
            .is_some_and(|grant| bool::from(grant.as_bytes().ct_eq(bearer_token.as_bytes())));
        if !valid_audience {
            return None;
        }
        if control {
            Some(self.control_origin.clone())
        } else if privileged {
            Some(self.privileged_origin.clone())
        } else {
            None
        }
    }

    fn permits_schema(&self, origin: &AuthenticatedCommandOrigin, resource: &ResourceRef) -> bool {
        resource.resource.is_none()
            && resource.native_protocol_reference.is_none()
            && self.stations.contains(&resource.station_id)
            && (origin == &self.control_origin
                || (self.privileged.is_some() && origin == &self.privileged_origin))
    }

    fn start_reference(
        &self,
        origin: &AuthenticatedCommandOrigin,
        resource: &ResourceRef,
    ) -> Option<String> {
        (origin == &self.control_origin && self.permits_schema(origin, resource))
            .then(|| self.start_refs.get(&resource.station_id).cloned())
            .flatten()
    }
}

struct PinnedPayloads;
impl PrivilegedPayloadValidator for PinnedPayloads {
    fn validate(
        &self,
        _: &uob_contracts::PrivilegedOcppOperation<Value>,
    ) -> Result<(), &'static str> {
        Err("command.unsupported_schema")
    }
    fn validate_resource(
        &self,
        resource: &ResourceRef,
        operation: &uob_contracts::PrivilegedOcppOperation<Value>,
    ) -> Result<(), &'static str> {
        uob_protocol_adapter::command_registry::validate_privileged_operation(resource, operation)
            .map_err(|_| "command.invalid_schema_or_payload")
    }
    fn schemas(&self, snapshot: &uob_contracts::StationSnapshot) -> Vec<Value> {
        uob_protocol_adapter::command_registry::command_schemas(snapshot)
            .into_iter()
            .filter_map(|schema| serde_json::to_value(schema).ok())
            .collect()
    }
    fn offers(
        &self,
        snapshot: &uob_contracts::StationSnapshot,
        resource: &ResourceRef,
        operation: &uob_contracts::PrivilegedOcppOperation<Value>,
    ) -> bool {
        uob_protocol_adapter::command_registry::command_schemas(snapshot)
            .iter()
            .any(|schema| {
                &schema.resource == resource
                    && schema.protocol == operation.protocol
                    && schema.action == operation.action.as_str()
                    && schema.payload_schema == operation.payload_schema.as_str()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uob_contracts::{BridgeId, StationId};

    fn credential(letter: char) -> ReadGrant {
        super::super::files::grant(
            format!("uob1.demo.{}", letter.to_string().repeat(32)).into_bytes(),
        )
    }

    #[test]
    fn read_control_and_privileged_credentials_never_share_command_permissions() {
        let station = ResourceRef {
            bridge_id: BridgeId::new("bridge-a").unwrap(),
            station_id: StationId::new("station-a").unwrap(),
            resource: None,
            native_protocol_reference: None,
        };
        let mut references = BTreeMap::new();
        references.insert(
            station.station_id.clone(),
            "sha256:opaque-reference".to_owned(),
        );
        let authenticator = ControlCredentials::new(
            Environment::Demo,
            &credential('a'),
            credential('b'),
            Some(credential('c')),
            std::slice::from_ref(&station),
            references,
        )
        .unwrap();
        assert!(
            authenticator
                .authenticate(&format!("uob1.demo.{}", "a".repeat(32)))
                .is_none()
        );
        let control = authenticator
            .authenticate(&format!("uob1.demo.{}", "b".repeat(32)))
            .unwrap();
        let privileged = authenticator
            .authenticate(&format!("uob1.demo.{}", "c".repeat(32)))
            .unwrap();
        assert_ne!(control, privileged);
        assert!(authenticator.permits_schema(&control, &station));
        assert!(authenticator.permits_schema(&privileged, &station));
        assert_eq!(
            authenticator.start_reference(&control, &station).as_deref(),
            Some("sha256:opaque-reference")
        );
        assert!(
            authenticator
                .start_reference(&privileged, &station)
                .is_none()
        );
        let other = ResourceRef {
            station_id: StationId::new("station-b").unwrap(),
            ..station
        };
        assert!(!authenticator.permits_schema(&control, &other));
        assert!(authenticator.start_reference(&control, &other).is_none());
    }
}
