use super::{ConfiguredOcppVersion, RunFailure, StationDefinition, setup_failure};
use std::collections::HashSet;

pub(super) fn validate_station_topology(station: &StationDefinition) -> Result<(), RunFailure> {
    if (station.ocpp_version == ConfiguredOcppVersion::V1_6 && station.reservation201.is_some())
        || (station.ocpp_version == ConfiguredOcppVersion::V2_0_1
            && station.reservation16.is_some())
    {
        return Err(setup_failure(
            "invalid_reservation_edition",
            "native reservation state must match the station OCPP edition",
        ));
    }
    if let Some(firmware) = &station.firmware16
        && (station.ocpp_version != ConfiguredOcppVersion::V1_6 || firmware.validate().is_err())
    {
        return Err(setup_failure(
            "invalid_firmware_configuration",
            "native firmware state requires a valid OCPP 1.6 station configuration",
        ));
    }
    if let Some(diagnostics) = &station.diagnostics16
        && (station.ocpp_version != ConfiguredOcppVersion::V1_6 || diagnostics.validate().is_err())
    {
        return Err(setup_failure(
            "invalid_diagnostics_configuration",
            "native diagnostics state requires a valid OCPP 1.6 station configuration",
        ));
    }
    if let Some(security) = &station.security16
        && (station.ocpp_version != ConfiguredOcppVersion::V1_6 || security.validate().is_err())
    {
        return Err(setup_failure(
            "invalid_security_configuration",
            "native security state requires a valid OCPP 1.6 station configuration",
        ));
    }
    if let Some(diagnostics) = &station.diagnostics201
        && (station.ocpp_version != ConfiguredOcppVersion::V2_0_1
            || diagnostics.validate().is_err())
    {
        return Err(setup_failure(
            "invalid_diagnostics_configuration",
            "native diagnostics state requires a valid OCPP 2.0.1 station configuration",
        ));
    }
    if let Some(monitoring) = &station.monitoring201
        && (station.ocpp_version != ConfiguredOcppVersion::V2_0_1 || monitoring.validate().is_err())
    {
        return Err(setup_failure(
            "invalid_monitoring_configuration",
            "native monitoring state requires a valid OCPP 2.0.1 station configuration",
        ));
    }
    if let Some(firmware) = &station.firmware201
        && (station.ocpp_version != ConfiguredOcppVersion::V2_0_1 || firmware.validate().is_err())
    {
        return Err(setup_failure(
            "invalid_firmware_configuration",
            "native firmware state requires a valid OCPP 2.0.1 station configuration",
        ));
    }
    match station.ocpp_version {
        ConfiguredOcppVersion::V1_6 if !station.evses.is_empty() => Err(setup_failure(
            "invalid_station_topology",
            "OCPP 1.6 stations use connector identities, not EVSE identities",
        )),
        ConfiguredOcppVersion::V2_0_1 if !station.connectors.is_empty() => Err(setup_failure(
            "invalid_station_topology",
            "OCPP 2.0.1 stations use EVSE and connector identity pairs",
        )),
        ConfiguredOcppVersion::V1_6 => validate_unique_nonzero(
            &station.connector_ids(),
            "OCPP 1.6 connector identifiers must be nonzero and unique",
        ),
        ConfiguredOcppVersion::V2_0_1 => {
            if station.evses.is_empty() {
                return Ok(());
            }
            let mut evse_ids = HashSet::new();
            for evse in &station.evses {
                if evse.id == 0 || !evse_ids.insert(evse.id) || evse.connectors.is_empty() {
                    return Err(setup_failure(
                        "invalid_station_topology",
                        "OCPP 2.0.1 EVSE identifiers must be nonzero and unique with connectors",
                    ));
                }
                validate_unique_nonzero(
                    &evse.connectors,
                    "OCPP 2.0.1 connector identifiers must be nonzero and unique within an EVSE",
                )?;
            }
            Ok(())
        }
    }
}

fn validate_unique_nonzero(values: &[u16], message: &'static str) -> Result<(), RunFailure> {
    let mut seen = HashSet::new();
    if values
        .iter()
        .any(|value| *value == 0 || !seen.insert(*value))
    {
        Err(setup_failure("invalid_station_topology", message))
    } else {
        Ok(())
    }
}
