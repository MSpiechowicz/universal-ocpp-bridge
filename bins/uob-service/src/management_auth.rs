//! Station-scoped management event bearer authentication.
//!
//! Credential file loading and trusted roster selection at the service boundary belong to the
//! caller. This module revalidates the roster and creates only explicit station grants.

use std::{collections::BTreeSet, fmt};

use subtle::ConstantTimeEq;
use uob_application::{TargetQueryAuthorization, TargetQueryPermission, TargetResourceScope};
use uob_contracts::{BridgeId, Environment, ResourceRef, TargetInstanceId};
use uob_management_adapter::{AuthenticatedEventAccess, token_matches_environment};

/// A protected reader for one target and a fixed, nonempty station roster.
///
/// No `Debug` representation contains the bearer secret. The authenticator is immutable once
/// constructed; all authenticated requests receive the same narrowly scoped read grant.
pub(crate) struct ManagementEventAuthenticator {
    environment: Environment,
    token: Vec<u8>,
    access: AuthenticatedEventAccess,
}

impl fmt::Debug for ManagementEventAuthenticator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagementEventAuthenticator")
            .field("token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// Sanitized startup failure; never carries credential bytes or a resource identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagementAuthError {
    InvalidCredential,
    InvalidGrant,
}

impl ManagementEventAuthenticator {
    /// Constructs a reader for exactly the supplied station resources on `bridge_id` and
    /// `target_instance_id`. `default_resource` must be a member of the roster. Each resource
    /// must be station-level with no native protocol address, and duplicates are rejected.
    ///
    /// `token` is the entire bearer credential read by the caller from a protected file. It must
    /// have valid syntax and the audience of `environment`; this constructor does not read paths
    /// or produce diagnostics containing the credential. Caller supplies the trusted selected
    /// target identity and validated station roster, not user-controlled request fields.
    pub(crate) fn new(
        environment: Environment,
        bridge_id: BridgeId,
        target_instance_id: TargetInstanceId,
        station_resources: Vec<ResourceRef>,
        default_resource: ResourceRef,
        token: Vec<u8>,
    ) -> Result<Self, ManagementAuthError> {
        if !std::str::from_utf8(&token)
            .is_ok_and(|token| token_matches_environment(token, environment))
        {
            return Err(ManagementAuthError::InvalidCredential);
        }
        let station_only = move |resource: &ResourceRef| station_only(resource, &bridge_id);

        if station_resources.is_empty() || !station_only(&default_resource) {
            return Err(ManagementAuthError::InvalidGrant);
        }
        let mut stations = BTreeSet::new();
        let mut scopes = Vec::with_capacity(station_resources.len());
        let mut default_is_granted = false;
        for resource in station_resources {
            if !station_only(&resource) || !stations.insert(resource.station_id.clone()) {
                return Err(ManagementAuthError::InvalidGrant);
            }
            default_is_granted |= resource == default_resource;
            scopes.push(TargetResourceScope::Station {
                bridge_id: resource.bridge_id,
                station_id: resource.station_id,
            });
        }
        if !default_is_granted {
            return Err(ManagementAuthError::InvalidGrant);
        }

        Ok(Self {
            environment,
            token,
            access: AuthenticatedEventAccess {
                authorization: TargetQueryAuthorization::new(
                    target_instance_id,
                    vec![
                        TargetQueryPermission::StationSnapshots,
                        TargetQueryPermission::RetainedEvents,
                    ],
                    scopes,
                ),
                default_resource,
            },
        })
    }
}

fn station_only(resource: &ResourceRef, bridge_id: &BridgeId) -> bool {
    resource.bridge_id == *bridge_id
        && resource.resource.is_none()
        && resource.native_protocol_reference.is_none()
}

impl uob_management_adapter::ManagementEventAuthenticator for ManagementEventAuthenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedEventAccess> {
        (bool::from(self.token.as_slice().ct_eq(token.as_bytes()))
            && token_matches_environment(token, self.environment))
        .then(|| self.access.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use uob_application::{
        CanonicalQuerySource, PageLimit, RetainedEventQuery, ScopedTargetQueryPort,
        TargetPortFuture, TargetQuery, TargetQueryPort, TargetQueryResult,
        TargetRetainedEventStream,
    };
    use uob_contracts::{
        CanonicalConnectorId, CanonicalResource, NativeProtocolReference, StationId,
    };
    use uob_management_adapter::ManagementEventAuthenticator as _;

    fn station(bridge: &str, name: &str) -> ResourceRef {
        ResourceRef {
            bridge_id: BridgeId::new(bridge).unwrap(),
            station_id: StationId::new(name).unwrap(),
            resource: None,
            native_protocol_reference: None,
        }
    }

    fn token() -> Vec<u8> {
        format!("uob1.demo.{}", "a".repeat(32)).into_bytes()
    }

    fn reader(roster: Vec<ResourceRef>, default: ResourceRef) -> ManagementEventAuthenticator {
        ManagementEventAuthenticator::new(
            Environment::Demo,
            BridgeId::new("bridge-a").unwrap(),
            TargetInstanceId::new("target-a").unwrap(),
            roster,
            default,
            token(),
        )
        .unwrap()
    }

    #[test]
    fn authenticates_only_the_full_correct_audience_token_and_redacts_debug() {
        let reader = reader(
            vec![station("bridge-a", "station-a")],
            station("bridge-a", "station-a"),
        );
        let valid = String::from_utf8(token()).unwrap();
        assert!(reader.authenticate(&valid).is_some());
        let wrong_secret = format!("uob1.demo.{}", "b".repeat(32));
        let wrong_audience = format!("uob1.staging.{}", "a".repeat(32));
        for candidate in [&wrong_secret, &wrong_audience, "invalid", ""] {
            assert!(reader.authenticate(candidate).is_none());
        }
        assert!(!format!("{reader:?}").contains(&valid));
        assert!(!format!("{reader:?}").contains(&"a".repeat(32)));
        assert!(matches!(
            ManagementEventAuthenticator::new(
                Environment::Production,
                BridgeId::new("bridge-a").unwrap(),
                TargetInstanceId::new("target-a").unwrap(),
                vec![station("bridge-a", "station-a")],
                station("bridge-a", "station-a"),
                token(),
            ),
            Err(ManagementAuthError::InvalidCredential)
        ));
    }

    #[test]
    fn rejects_empty_duplicate_foreign_and_non_station_grants_or_foreign_default() {
        let a = station("bridge-a", "station-a");
        let mut child = a.clone();
        child.resource = Some(CanonicalResource::Connector {
            connector_id: CanonicalConnectorId::new("connector-a").unwrap(),
        });
        let mut native = a.clone();
        native.native_protocol_reference =
            Some(NativeProtocolReference::Ocpp16 { connector_id: 1 });
        let invalid_rosters = [
            vec![],
            vec![a.clone(), a.clone()],
            vec![station("bridge-b", "station-a")],
            vec![child.clone()],
            vec![native.clone()],
        ];
        for roster in invalid_rosters {
            assert!(matches!(
                ManagementEventAuthenticator::new(
                    Environment::Demo,
                    a.bridge_id.clone(),
                    TargetInstanceId::new("target-a").unwrap(),
                    roster,
                    a.clone(),
                    token(),
                ),
                Err(ManagementAuthError::InvalidGrant)
            ));
        }
        for default in [
            child,
            native,
            station("bridge-a", "station-b"),
            station("bridge-b", "station-a"),
        ] {
            assert!(matches!(
                ManagementEventAuthenticator::new(
                    Environment::Demo,
                    a.bridge_id.clone(),
                    TargetInstanceId::new("target-a").unwrap(),
                    vec![a.clone()],
                    default,
                    token(),
                ),
                Err(ManagementAuthError::InvalidGrant)
            ));
        }
    }

    struct SnapshotSource(Arc<AtomicUsize>);

    impl CanonicalQuerySource<()> for SnapshotSource {
        fn query<'a>(
            &'a self,
            _: &'a TargetQueryAuthorization,
            _: TargetQuery,
        ) -> TargetPortFuture<'a, TargetQueryResult<()>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(TargetQueryResult::StationSnapshot(None)) })
        }

        fn subscribe_retained_events<'a>(
            &'a self,
            _: &'a TargetQueryAuthorization,
            _: RetainedEventQuery,
        ) -> TargetPortFuture<'a, TargetRetainedEventStream<()>> {
            panic!("this test does not subscribe")
        }
    }

    #[tokio::test]
    async fn grants_both_read_permissions_and_scoped_query_port_denies_foreign_stations() {
        let a = station("bridge-a", "station-a");
        let b = station("bridge-a", "station-b");
        let reader = reader(vec![a.clone(), b.clone()], b.clone());
        let grant = reader
            .authenticate(&String::from_utf8(token()).unwrap())
            .unwrap();
        assert_eq!(grant.default_resource, b);
        assert_eq!(
            grant.authorization.target_instance_id(),
            &TargetInstanceId::new("target-a").unwrap()
        );
        assert!(
            grant
                .authorization
                .permits(TargetQueryPermission::StationSnapshots)
        );
        assert!(
            grant
                .authorization
                .permits(TargetQueryPermission::RetainedEvents)
        );
        assert!(
            !grant
                .authorization
                .permits(TargetQueryPermission::DataPoints)
        );
        let queries = Arc::new(AtomicUsize::new(0));
        let port = ScopedTargetQueryPort::new(
            Arc::new(SnapshotSource(Arc::clone(&queries))),
            grant.authorization,
        );
        for permitted in [a.clone(), b] {
            assert!(matches!(
                port.query(TargetQuery::StationSnapshot(permitted)).await,
                Ok(TargetQueryResult::StationSnapshot(None))
            ));
        }
        for denied in [
            station("bridge-a", "station-c"),
            station("bridge-b", "station-a"),
        ] {
            assert_eq!(
                port.query(TargetQuery::StationSnapshot(denied))
                    .await
                    .unwrap_err()
                    .code(),
                uob_application::TargetPortErrorCode::Unauthorized
            );
        }
        assert_eq!(
            port.query(TargetQuery::RetainedEvents(RetainedEventQuery {
                resource: station("bridge-a", "station-c"),
                after: None,
                limit: PageLimit::new(1).unwrap(),
            }))
            .await
            .unwrap_err()
            .code(),
            uob_application::TargetPortErrorCode::Unauthorized
        );
        assert_eq!(queries.load(Ordering::SeqCst), 2);
    }
}
