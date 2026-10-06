//! OCPP 2.0.1 reservation capabilities stay default-off and independently explicit.
use super::super::{ChargingAuthorization, StationSettings};
use std::{collections::BTreeMap, io};
use uob_application::{
    AuthorizationChange, AuthorizationState,
    charging_identity::{ChargingIdentityProvider, ChargingIdentityResolution},
};
use uob_contracts::{
    CanonicalResource, NativeProtocolReference, Operation, ProtocolEdition, ResourceRef, StationId,
    SupportedOperation, UtcTimestamp,
};
use uob_provider_adapter::LocalChargingIdentityProvider;

fn operation(action: &str) -> SupportedOperation {
    SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
            action: action.to_owned(),
        },
        parameters: vec![],
    }
}
pub(super) fn apply_capabilities(
    snapshot: &mut uob_contracts::StationSnapshot,
    settings: &StationSettings,
) {
    if settings.control.cancel_reservation.enabled() {
        snapshot
            .capabilities
            .operations
            .push(operation("CancelReservation"));
    }
    if !settings.control.reserve_now.enabled() || settings.reservations_201.is_none() {
        return;
    }
    if settings.control.reserve_non_evse_specific_supported {
        snapshot
            .capabilities
            .operations
            .push(operation("ReserveNow"));
    }
    // EVSE reservations are offered only on EVSE resources, never on one connector.
    for entry in &mut snapshot.resources {
        if matches!(
            entry.resource.resource,
            Some(CanonicalResource::Evse {
                connector_id: None,
                ..
            })
        ) && matches!(
            entry.resource.native_protocol_reference,
            Some(NativeProtocolReference::Ocpp201 {
                evse_id: 1..,
                connector_id: None
            })
        ) {
            entry.capabilities.operations.push(operation("ReserveNow"));
        }
    }
}
pub(super) async fn provision_policy(
    authorization: &ChargingAuthorization,
    settings: &BTreeMap<StationId, StationSettings>,
    resources: &BTreeMap<StationId, Vec<ResourceRef>>,
) -> io::Result<()> {
    for (station, settings) in settings {
        let Some(provider) = &settings.reservations_201 else {
            continue;
        };
        let resource = resources
            .get(station)
            .and_then(|r| r.first())
            .ok_or_else(|| io::Error::other("reservation policy station unavailable"))?;
        for (identity, authorize, revision) in provider.policy_entries() {
            let Ok(ChargingIdentityResolution::Resolved { reference, .. }) =
                LocalChargingIdentityProvider.resolve(&identity).await
            else {
                return Err(io::Error::other("reservation policy identity unavailable"));
            };
            authorization
                .apply_change(AuthorizationChange {
                    reference,
                    resource: resource.clone(),
                    state: if authorize {
                        AuthorizationState::Active
                    } else {
                        AuthorizationState::Revoked
                    },
                    revision,
                    changed_at: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
                    expires_at: None,
                })
                .await
                .map_err(|_| io::Error::other("reservation policy provisioning unavailable"))?;
        }
    }
    Ok(())
}
