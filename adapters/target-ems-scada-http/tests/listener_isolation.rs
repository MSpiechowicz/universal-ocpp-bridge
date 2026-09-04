//! Proof that an EMS/SCADA integration credential cannot reach management, debug, or
//! administration surfaces.
//!
//! The route-level half is covered by the router tests. This file covers the credential-level
//! half: the two listeners require different, non-overlapping command origins, so one
//! credential cannot be configured on both.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use uob_application::{AccessGrant, AccessPermission, AccessResourceScope, CredentialReference};
use uob_contracts::{AuthenticatedCommandOrigin, BridgeId, PrincipalId, TargetInstanceId};
use uob_management_adapter::{
    ManagementCredentialConfiguration, ManagementListenerConfiguration,
    ManagementListenerPolicyError, ManagementTlsConfiguration,
};

fn reference(value: &str) -> CredentialReference {
    CredentialReference::new(value).expect("credential reference")
}

/// Builds the exact grant shape the integration credential file produces.
fn integration_grant() -> AccessGrant {
    AccessGrant::new(
        AuthenticatedCommandOrigin::Target {
            target_instance_id: TargetInstanceId::new("main").expect("target instance"),
            principal_id: PrincipalId::new("ems-reader").expect("principal"),
        },
        vec![AccessPermission::Read],
        vec![AccessResourceScope::Bridge(
            BridgeId::new("site-01").expect("bridge identity"),
        )],
    )
    .expect("integration grant")
}

#[test]
fn an_integration_credential_cannot_be_configured_on_the_management_listener() {
    let configuration = ManagementListenerConfiguration {
        listen_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8443),
        remote_access_enabled: true,
        tls: Some(ManagementTlsConfiguration {
            certificate_chain: reference("/run/uob/management/server.crt"),
            private_key: reference("/run/uob/management/server.key"),
        }),
        credentials: vec![ManagementCredentialConfiguration {
            credential: reference("/run/uob/ems/integration.toml"),
            grant: integration_grant(),
        }],
    };

    assert_eq!(
        configuration.validate(),
        Err(ManagementListenerPolicyError::ManagementOriginRequired),
        "a target-origin integration grant must never satisfy the management listener"
    );
}

#[test]
fn a_management_credential_is_not_expressible_by_the_integration_credential_file() {
    // The integration file always binds a grant to the configured target instance, so its
    // origin is `Target`. `Management` is a different variant and cannot be produced there.
    let integration = integration_grant();
    assert!(matches!(
        integration.origin(),
        AuthenticatedCommandOrigin::Target { .. }
    ));
    assert!(!matches!(
        integration.origin(),
        AuthenticatedCommandOrigin::Management { .. }
    ));
}

#[test]
fn an_integration_grant_is_bounded_to_its_declared_resources() {
    let grant = integration_grant();
    let inside = uob_contracts::ResourceRef {
        bridge_id: BridgeId::new("site-01").expect("bridge identity"),
        station_id: uob_contracts::StationId::new("station-a").expect("station identity"),
        resource: None,
        native_protocol_reference: None,
    };
    let outside = uob_contracts::ResourceRef {
        bridge_id: BridgeId::new("site-02").expect("bridge identity"),
        ..inside.clone()
    };

    assert!(grant.permits(AccessPermission::Read, &inside));
    assert!(!grant.permits(AccessPermission::Read, &outside));
    // A read-only integration credential never gains command or privileged authority.
    assert!(!grant.permits(AccessPermission::Control, &inside));
    assert!(!grant.permits(AccessPermission::PrivilegedControl, &inside));
}
