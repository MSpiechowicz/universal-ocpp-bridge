use std::{collections::BTreeMap, sync::Arc};

use serde_json::Value;
use subtle::ConstantTimeEq;
use uob_application::{
    AccessGrant, AccessPermission, AccessPolicy, AccessResourceScope, CommandAdmissionPort,
    ScopedCommandAdmissionPort,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CanonicalResource, Environment, NativeProtocolReference,
    PrincipalId, ResourceRef, StationId,
};
use uob_management_adapter::{
    ManagementCommandAuthenticator, ManagementCommandConfiguration, PrivilegedPayloadValidator,
    token_matches_environment,
};

use super::files::ReadGrant;
use uob_protocol_adapter::v201::remote_control::configuration201_values::LocalConfigurationValues201;

pub(super) struct ControlCredentials {
    environment: Environment,
    control: ReadGrant,
    privileged: Option<ReadGrant>,
    control_origin: AuthenticatedCommandOrigin,
    privileged_origin: AuthenticatedCommandOrigin,
    stations: Vec<ResourceRef>,
    connectors: Vec<ResourceRef>,
    start_refs: BTreeMap<StationId, String>,
}

impl ControlCredentials {
    pub(super) fn new(
        environment: Environment,
        read: &ReadGrant,
        control: ReadGrant,
        privileged: Option<ReadGrant>,
        resources: &BTreeMap<StationId, Vec<ResourceRef>>,
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
            stations: roster.to_vec(),
            connectors: resources
                .values()
                .flat_map(|entries| entries.iter().skip(1))
                .filter(|resource| {
                    matches!(
                        (&resource.resource, &resource.native_protocol_reference),
                        (
                            Some(CanonicalResource::Connector { .. }),
                            Some(NativeProtocolReference::Ocpp16 { connector_id: 1.. })
                        ) | (
                            Some(CanonicalResource::Evse { .. }),
                            Some(NativeProtocolReference::Ocpp201 { evse_id: 1.., .. })
                        )
                    )
                })
                .cloned()
                .collect(),
            start_refs,
        })
    }

    pub(super) fn configuration(
        self: Arc<Self>,
        roster: &[ResourceRef],
        inner: Arc<dyn CommandAdmissionPort<Value>>,
        protected: Option<Arc<LocalConfigurationValues201>>,
        local_authorization: super::local_authorization::Providers,
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
            privileged_payloads: Arc::new(PinnedPayloads {
                protected,
                local_authorization,
            }),
        })
    }
    pub(super) fn reservation_grant(
        self: &Arc<Self>,
    ) -> Arc<uob_protocol_adapter::v16::remote_control::ReservationGrant16> {
        let owner = self.clone();
        Arc::new(move |command, now| {
            owner.privileged.is_some()
                && command.origin == owner.privileged_origin
                && owner.permits_schema(&command.origin, &command.resource)
                && now < command.expires_at
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
        if resource.resource.is_none() && resource.native_protocol_reference.is_none() {
            return self.stations.contains(resource)
                && (origin == &self.control_origin
                    || (self.privileged.is_some() && origin == &self.privileged_origin));
        }

        self.privileged.is_some()
            && origin == &self.privileged_origin
            && self.connectors.contains(resource)
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

struct PinnedPayloads {
    protected: Option<Arc<LocalConfigurationValues201>>,
    local_authorization: super::local_authorization::Providers,
}
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
        let offered = uob_protocol_adapter::command_registry::command_schemas(snapshot)
            .iter()
            .any(|schema| {
                &schema.resource == resource
                    && schema.protocol == operation.protocol
                    && schema.action == operation.action.as_str()
                    && schema.payload_schema == operation.payload_schema.as_str()
            });
        if !offered {
            return false;
        }
        if operation.action.as_str() == "SendLocalList" {
            if operation.protocol == uob_contracts::ProtocolEdition::Ocpp201 {
                return self
                    .local_authorization
                    .v201
                    .as_ref()
                    .is_some_and(|provider| {
                        serde_json::from_value::<uob_contracts::SendLocalListReference201>(
                            operation.payload.clone(),
                        )
                        .is_ok_and(|request| {
                            provider.authorized(
                                resource,
                                &request,
                                uob_contracts::UtcTimestamp::new(time::OffsetDateTime::now_utc()),
                            )
                        })
                    });
            }
            return self
                .local_authorization
                .v16
                .as_ref()
                .is_some_and(|provider| {
                    serde_json::from_value::<uob_contracts::SendLocalListReference16>(
                        operation.payload.clone(),
                    )
                    .is_ok_and(|request| {
                        provider.authorized(
                            resource,
                            &request,
                            uob_contracts::UtcTimestamp::new(time::OffsetDateTime::now_utc()),
                        )
                    })
                });
        }
        if matches!(
            operation.action.as_str(),
            "SetVariables" | "SetNetworkProfile"
        ) {
            // Offers is evaluated only for fresh commands after the existing scoped
            // durable retry hint. Recheck all references again at the actual socket send.
            self.protected.as_ref().is_some_and(|provider| {
                provider
                    .validate_operation(
                        resource,
                        operation,
                        uob_contracts::UtcTimestamp::new(time::OffsetDateTime::now_utc()),
                    )
                    .is_ok()
            })
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uob_contracts::{BridgeId, CanonicalConnectorId, StationId};

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
        let resources = BTreeMap::from([(station.station_id.clone(), vec![station.clone()])]);
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
            &resources,
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

    #[test]
    fn only_privileged_identity_can_discover_exact_rostered_native_connectors() {
        let station = ResourceRef {
            bridge_id: BridgeId::new("bridge-a").unwrap(),
            station_id: StationId::new("station-a").unwrap(),
            resource: None,
            native_protocol_reference: None,
        };
        let connector = ResourceRef {
            resource: Some(CanonicalResource::Connector {
                connector_id: CanonicalConnectorId::new("connector-1").unwrap(),
            }),
            native_protocol_reference: Some(NativeProtocolReference::Ocpp16 { connector_id: 1 }),
            ..station.clone()
        };
        let resources = BTreeMap::from([(
            station.station_id.clone(),
            vec![station.clone(), connector.clone()],
        )]);
        let authenticator = ControlCredentials::new(
            Environment::Demo,
            &credential('a'),
            credential('b'),
            Some(credential('c')),
            &resources,
            std::slice::from_ref(&station),
            BTreeMap::new(),
        )
        .unwrap();
        let control = authenticator
            .authenticate(&format!("uob1.demo.{}", "b".repeat(32)))
            .unwrap();
        let privileged = authenticator
            .authenticate(&format!("uob1.demo.{}", "c".repeat(32)))
            .unwrap();
        assert!(authenticator.permits_schema(&privileged, &connector));
        assert!(!authenticator.permits_schema(&control, &connector));
        assert!(
            authenticator
                .start_reference(&control, &connector)
                .is_none()
        );

        let mut wrong = connector.clone();
        wrong.bridge_id = BridgeId::new("bridge-b").unwrap();
        assert!(!authenticator.permits_schema(&privileged, &wrong));
        wrong = connector.clone();
        wrong.station_id = StationId::new("station-b").unwrap();
        assert!(!authenticator.permits_schema(&privileged, &wrong));
        wrong = connector.clone();
        wrong.resource = Some(CanonicalResource::Connector {
            connector_id: CanonicalConnectorId::new("connector-2").unwrap(),
        });
        assert!(!authenticator.permits_schema(&privileged, &wrong));
        wrong = connector.clone();
        wrong.native_protocol_reference = Some(NativeProtocolReference::Ocpp16 { connector_id: 2 });
        assert!(!authenticator.permits_schema(&privileged, &wrong));
        wrong = station.clone();
        wrong.bridge_id = BridgeId::new("bridge-b").unwrap();
        assert!(!authenticator.permits_schema(&control, &wrong));
        assert!(!authenticator.permits_schema(&privileged, &wrong));
        wrong = station;
        wrong.native_protocol_reference = Some(NativeProtocolReference::Ocpp16 { connector_id: 0 });
        assert!(!authenticator.permits_schema(&privileged, &wrong));
    }
}
