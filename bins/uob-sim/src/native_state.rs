use crate::{
    OcppVersion, SimulatorClientConfig, SimulatorClientError, local_authorization,
    local_authorization201, reservation201,
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

pub(crate) fn open_firmware16(
    config: &SimulatorClientConfig,
) -> Result<Option<crate::firmware16::FirmwareHandle>, SimulatorClientError> {
    config
        .firmware16
        .as_ref()
        .map(|(station, options)| {
            crate::firmware16::FirmwareHandle::open(station, options)
                .map_err(|code| SimulatorClientError::Protocol(code.to_owned()))
        })
        .transpose()
}

pub(crate) fn open_firmware201(
    config: &SimulatorClientConfig,
) -> Result<Option<crate::firmware201::Firmware201Handle>, SimulatorClientError> {
    config
        .firmware201
        .as_ref()
        .map(|(station, options)| {
            crate::firmware201::Firmware201Handle::open(station, options)
                .map_err(|code| SimulatorClientError::Protocol(code.to_owned()))
        })
        .transpose()
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

pub(crate) fn validate_client_config(
    config: &SimulatorClientConfig,
) -> Result<(), SimulatorClientError> {
    if config.command_capacity == 0 {
        return Err(SimulatorClientError::InvalidCapacity("command_capacity"));
    }
    if config.trace_capacity == 0 {
        return Err(SimulatorClientError::InvalidCapacity("trace_capacity"));
    }
    if config.version != OcppVersion::V1_6 && config.local_authorization.is_some() {
        return Err(SimulatorClientError::Protocol(
            "OCPP 1.6 handle cannot attach to OCPP 2.0.1".to_owned(),
        ));
    }
    if config.version != OcppVersion::V1_6 && config.reservation16.is_some() {
        return Err(SimulatorClientError::Protocol(
            "OCPP 1.6 reservations cannot attach to OCPP 2.0.1".to_owned(),
        ));
    }
    if config.version != OcppVersion::V1_6 && config.firmware16.is_some() {
        return Err(SimulatorClientError::Protocol(
            "OCPP 1.6 firmware cannot attach to OCPP 2.0.1".to_owned(),
        ));
    }
    if config.version != OcppVersion::V2_0_1 && config.firmware201.is_some() {
        return Err(SimulatorClientError::Protocol(
            "OCPP 2.0.1 firmware cannot attach to OCPP 1.6".to_owned(),
        ));
    }
    // A firmware reboot closes the socket; only a reconnecting station can report Installed.
    if config
        .firmware201
        .as_ref()
        .is_some_and(|(_, firmware)| firmware.reboot_after_install)
        && !config.reconnect
    {
        return Err(SimulatorClientError::Protocol(
            "OCPP 2.0.1 firmware reboot requires reconnect".to_owned(),
        ));
    }
    if config.version != OcppVersion::V2_0_1 && config.reservation201.is_some() {
        return Err(SimulatorClientError::Protocol(
            "OCPP 2.0.1 reservations cannot attach to OCPP 1.6".to_owned(),
        ));
    }
    Ok(())
}
