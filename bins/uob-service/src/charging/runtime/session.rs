use std::{io, sync::Arc};

use uob_contracts::{ProtocolEdition, StationId, StationSnapshot};
use uob_protocol_adapter::CallSessionHandle;
use uob_provider_adapter::LocalChargingIdentityProvider;

use super::{Clock, StationContext, unavailable};
use crate::charging::StationSettings;

pub(super) async fn attach(
    context: &StationContext,
    station: &StationId,
    protocol: ProtocolEdition,
    configuration: &StationSettings,
    handle: &CallSessionHandle,
    snapshot: &StationSnapshot,
) -> io::Result<Option<u64>> {
    if !context.commands_enabled {
        return Ok(None);
    }

    let generation = match protocol {
        ProtocolEdition::Ocpp16j => {
            let start = match &configuration.start {
                Some(start) => start.v16.clone().ok_or_else(unavailable)?,
                None => Arc::new(
                    uob_protocol_adapter::v16::remote_control::LocalRemoteStartIdentity::new(
                        vec![],
                        context.authorization.clone(),
                    )
                    .map_err(|_| unavailable())?,
                ),
            };
            let session = Arc::new(
                uob_protocol_adapter::v16::remote_control::RemoteControlSession::new(
                    handle.clone(),
                    snapshot.clone(),
                    start,
                    Arc::new(Clock),
                    Arc::new(context.store.clone()),
                )
                .map_err(|_| unavailable())?,
            );
            context.commands.attach_16(station.clone(), session)
        }
        ProtocolEdition::Ocpp201 => {
            let start = match &configuration.start {
                Some(start) => start.v201.clone().ok_or_else(unavailable)?,
                None => Arc::new(
                    uob_protocol_adapter::v201::remote_control::LocalRemoteStartIdentity::new(
                        vec![],
                        &LocalChargingIdentityProvider,
                        context.authorization.clone(),
                    )
                    .await
                    .map_err(|_| unavailable())?,
                ),
            };
            let session = Arc::new(
                uob_protocol_adapter::v201::remote_control::RemoteControlSession::new(
                    handle.clone(),
                    snapshot.clone(),
                    start,
                    Arc::new(Clock),
                    Arc::new(context.store.clone()),
                )
                .map_err(|_| unavailable())?,
            );
            context.commands.attach_201(station.clone(), session)
        }
    };
    Ok(Some(generation))
}
