use std::{collections::BTreeMap, io, sync::Arc};

use uob_application::{
    AuthorizationChange, AuthorizationProvider, AuthorizationState, SensitiveAuthorizationToken,
    charging_identity::{
        ChargingIdentityProvider, ChargingIdentityResolution, ChargingTokenKind,
        PresentedChargingIdentity,
    },
};
use uob_contracts::{ProtocolEdition, StationId, UtcTimestamp};
use uob_protocol_adapter::{v16, v201};
use uob_provider_adapter::{LocalAuthorizationProvider, LocalChargingIdentityProvider};

use super::{ChargingAuthorization, files::ReadGrant};

#[derive(Clone)]
pub(super) struct StartIdentity {
    pub reference: String,
    pub v16: Option<Arc<dyn v16::remote_control::RemoteStartIdentity>>,
    pub v201: Option<Arc<dyn v201::remote_control::RemoteStartIdentity>>,
}

pub(super) async fn provision(
    authorization: &Arc<ChargingAuthorization>,
    stations: &[(
        StationId,
        ProtocolEdition,
        uob_contracts::ResourceRef,
        ReadGrant,
    )],
) -> io::Result<BTreeMap<StationId, StartIdentity>> {
    let mut identities = BTreeMap::new();
    for (station, protocol, resource, grant) in stations {
        let mut bytes = grant.token();
        let token = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
        if token.is_empty() || token.chars().any(char::is_control) || token.trim() != token {
            bytes.fill(0);
            return Err(invalid());
        }
        let (reference, v16, v201) = match protocol {
            ProtocolEdition::Ocpp16j => {
                let sensitive = SensitiveAuthorizationToken::new(&bytes).map_err(|_| invalid())?;
                let reference = LocalAuthorizationProvider
                    .resolve(&sensitive)
                    .await
                    .map_err(|_| invalid())?;
                let identity = v16::remote_control::LocalRemoteStartIdentity::new(
                    vec![sensitive],
                    authorization.clone(),
                )
                .map_err(|_| invalid())?;
                (
                    reference,
                    Some(Arc::new(identity) as Arc<dyn v16::remote_control::RemoteStartIdentity>),
                    None,
                )
            }
            ProtocolEdition::Ocpp201 => {
                let presented = PresentedChargingIdentity {
                    token: token.to_owned(),
                    kind: ChargingTokenKind::Local,
                    additional: vec![],
                    certificate: None,
                    certificate_hashes: vec![],
                };
                let ChargingIdentityResolution::Resolved { reference, .. } =
                    LocalChargingIdentityProvider
                        .resolve(&presented)
                        .await
                        .map_err(|_| invalid())?
                else {
                    return Err(invalid());
                };
                let identity = v201::remote_control::LocalRemoteStartIdentity::new(
                    vec![presented],
                    &LocalChargingIdentityProvider,
                    authorization.clone(),
                )
                .await
                .map_err(|_| invalid())?;
                (
                    reference,
                    None,
                    Some(Arc::new(identity) as Arc<dyn v201::remote_control::RemoteStartIdentity>),
                )
            }
        };
        bytes.fill(0);
        // Stable idempotent revision survives restart; policy changes must be explicit and cannot
        // be silently overwritten by a modified configuration or a stale runtime.
        authorization
            .apply_change(AuthorizationChange {
                reference: reference.clone(),
                resource: resource.clone(),
                state: AuthorizationState::Active,
                revision: 1,
                changed_at: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
                expires_at: None,
            })
            .await
            .map_err(|_| invalid())?;
        identities.insert(
            station.clone(),
            StartIdentity {
                reference: reference.as_str().to_owned(),
                v16,
                v201,
            },
        );
    }
    Ok(identities)
}

fn invalid() -> io::Error {
    io::Error::other("charging start identity unavailable")
}
