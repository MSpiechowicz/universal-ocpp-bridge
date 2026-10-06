//! Default-off reservation capabilities and independently explicit authorization provisioning.
mod late;
mod v201;
use super::{ChargingAuthorization, StationSettings};
pub(super) use late::late_response;
use std::{collections::BTreeMap, io};
use uob_application::{AuthorizationChange, AuthorizationProvider, AuthorizationState};
use uob_contracts::{
    Operation, ProtocolEdition, ResourceRef, StationId, SupportedOperation, UtcTimestamp,
};

pub(super) fn apply_capabilities(
    snapshot: &mut uob_contracts::StationSnapshot,
    settings: &StationSettings,
) {
    if settings.protocol == ProtocolEdition::Ocpp201 {
        v201::apply_capabilities(snapshot, settings);
        return;
    }
    let operation = |action: &str| SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: action.to_owned(),
        },
        parameters: vec![],
    };
    if settings.control.cancel_reservation.enabled() {
        snapshot
            .capabilities
            .operations
            .push(operation("CancelReservation"));
    }
    if settings.control.reserve_now.enabled() && settings.reservations.is_some() {
        if settings.control.reserve_connector_zero_supported {
            snapshot
                .capabilities
                .operations
                .push(operation("ReserveNow"));
        }
        for entry in &mut snapshot.resources {
            if matches!(
                entry.resource.native_protocol_reference,
                Some(uob_contracts::NativeProtocolReference::Ocpp16 { connector_id: 1.. })
            ) {
                entry.capabilities.operations.push(operation("ReserveNow"));
            }
        }
    }
}
pub(super) async fn provision_policy(
    authorization: &ChargingAuthorization,
    settings: &BTreeMap<StationId, StationSettings>,
    resources: &BTreeMap<StationId, Vec<ResourceRef>>,
) -> io::Result<()> {
    for (station, settings) in settings {
        let Some(provider) = &settings.reservations else {
            continue;
        };
        let resource = resources
            .get(station)
            .and_then(|r| r.first())
            .ok_or_else(|| io::Error::other("reservation policy station unavailable"))?;
        for (token, authorize, revision) in provider.policy_entries() {
            let reference = uob_provider_adapter::LocalAuthorizationProvider
                .resolve(token)
                .await
                .map_err(|_| io::Error::other("reservation policy identity unavailable"))?;
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
    v201::provision_policy(authorization, settings, resources).await
}
