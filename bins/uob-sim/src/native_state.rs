use crate::{
    SimulatorClientConfig, SimulatorClientError, local_authorization, local_authorization201,
    reservation201,
};

pub(crate) fn open_native_state(
    config: &SimulatorClientConfig,
) -> Result<local_authorization::LocalAuthorizationHandle, SimulatorClientError> {
    match (
        &config.local_authorization,
        &config.local_authorization_file,
    ) {
        (Some(handle), _) if !handle.has_persistence() => Err(SimulatorClientError::Protocol(
            "persistent native state required".to_owned(),
        )),
        (Some(handle), Some((station, _))) if !handle.station_matches(station) => Err(
            SimulatorClientError::Protocol("private native state binding mismatch".to_owned()),
        ),
        (Some(handle), _) => Ok(handle.clone()),
        (None, Some((station, settings))) => {
            local_authorization::LocalAuthorizationHandle::open(station, settings)
                .map_err(|code| SimulatorClientError::Protocol(code.to_owned()))
        }
        (None, None) => Ok(local_authorization::LocalAuthorizationHandle::unsupported(
            &config.endpoint,
        )),
    }
}

pub(crate) fn open_native_state201(
    config: &SimulatorClientConfig,
) -> Result<local_authorization201::LocalAuthorization201Handle, SimulatorClientError> {
    match &config.local_authorization_file {
        Some((station, settings)) => {
            local_authorization201::LocalAuthorization201Handle::open(station, settings)
                .map_err(|code| SimulatorClientError::Protocol(code.to_owned()))
        }
        None => {
            Ok(local_authorization201::LocalAuthorization201Handle::unsupported(&config.endpoint))
        }
    }
}

pub(crate) fn open_reservation201(
    config: &SimulatorClientConfig,
) -> Result<Option<reservation201::Reservation201Handle>, SimulatorClientError> {
    config
        .reservation201
        .as_ref()
        .map(|(station, options)| {
            reservation201::Reservation201Handle::open(station, &config.evse_connectors, options)
                .map_err(|code| SimulatorClientError::Protocol(code.to_owned()))
        })
        .transpose()
}
