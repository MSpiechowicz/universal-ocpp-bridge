use std::{io, sync::Arc};

use uob_contracts::{ProtocolEdition, StationId, StationSnapshot};
use uob_protocol_adapter::CallSessionHandle;
use uob_provider_adapter::LocalChargingIdentityProvider;

use super::{Clock, StationContext, unavailable};
use crate::charging::{StationSettings, firmware};

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
            let session = uob_protocol_adapter::v16::remote_control::RemoteControlSession::new(
                handle.clone(),
                snapshot.clone(),
                start,
                Arc::new(Clock),
                Arc::new(context.store.clone()),
            )
            .map_err(|_| unavailable())?;
            let session = match &configuration.local_authorization {
                Some(provider) => session.with_local_authorization_updates(provider.clone()),
                None => session,
            };
            let session = match &context.credentials {
                Some(credentials) => protected_16(session, context, configuration, credentials),
                None => session,
            };
            let session = Arc::new(session);
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
            let session = uob_protocol_adapter::v201::remote_control::RemoteControlSession::new(
                handle.clone(),
                snapshot.clone(),
                start,
                Arc::new(Clock),
                Arc::new(context.store.clone()),
            )
            .map_err(|_| unavailable())?;
            let session = match &configuration.configuration {
                Some(provider) => session.with_configuration_201(provider.clone()),
                None => session,
            };
            let session = match &configuration.local_authorization_201 {
                Some(provider) => session.with_local_authorization_updates(provider.clone()),
                None => session,
            };
            let session = match &context.credentials {
                Some(credentials) => protected_201(session, context, configuration, credentials),
                None => session,
            };
            context
                .commands
                .attach_201(station.clone(), session, context.store.clone())
        }
    };
    Ok(Some(generation))
}

/// Installs the privileged OCPP 1.6 workflows that need the shared control credentials.
fn protected_16(
    session: uob_protocol_adapter::v16::remote_control::RemoteControlSession,
    context: &StationContext,
    configuration: &StationSettings,
    credentials: &Arc<super::super::control_auth::ControlCredentials>,
) -> uob_protocol_adapter::v16::remote_control::RemoteControlSession {
    let session = session.with_reservations_16(
        configuration.reservations.clone(),
        configuration.control.reserve_connector_zero_supported,
        credentials.reservation_grant(),
    );
    let session = match (configuration.firmware, &configuration.firmware_providers) {
        (Some(firmware), Some(providers)) => session.with_firmware_16(firmware::session(
            firmware,
            providers,
            credentials.reservation_grant(),
        )),
        _ => session,
    };
    match (configuration.diagnostics, &configuration.firmware_providers) {
        (Some(diagnostics), Some(providers)) => {
            session.with_diagnostics_16(super::super::diagnostics::session(
                diagnostics,
                providers,
                Arc::new(context.store.clone()),
                credentials.reservation_grant(),
            ))
        }
        _ => session,
    }
}

/// Installs the privileged OCPP 2.0.1 workflows that need the shared control credentials.
fn protected_201(
    session: uob_protocol_adapter::v201::remote_control::RemoteControlSession,
    context: &StationContext,
    configuration: &StationSettings,
    credentials: &Arc<super::super::control_auth::ControlCredentials>,
) -> uob_protocol_adapter::v201::remote_control::RemoteControlSession {
    let session = session.with_reservations_201(
        configuration.reservations_201.clone(),
        configuration.control.reserve_non_evse_specific_supported,
        credentials.reservation_grant(),
    );
    let session = match (configuration.firmware, &configuration.firmware_providers) {
        (Some(firmware), Some(providers)) => session.with_firmware_201(firmware::session_201(
            firmware,
            providers,
            credentials.reservation_grant(),
        )),
        _ => session,
    };
    match (configuration.diagnostics, &configuration.firmware_providers) {
        (Some(diagnostics), Some(providers)) => {
            session.with_diagnostics_201(super::super::diagnostics::session_201(
                diagnostics,
                providers,
                Arc::new(context.store.clone()),
                credentials.reservation_grant(),
            ))
        }
        _ => session,
    }
}
