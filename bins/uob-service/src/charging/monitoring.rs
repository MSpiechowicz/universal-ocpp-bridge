//! OCPP 2.0.1 variable-monitoring actions (N02 to N06). They target the station root only,
//! because monitors belong to the charger's own device model.
use crate::configuration::charging::StationControlOptions;
use uob_contracts::{Operation, ProtocolEdition, StationSnapshot, SupportedOperation};

pub(super) fn apply(
    snapshot: &mut StationSnapshot,
    protocol: ProtocolEdition,
    options: StationControlOptions,
) {
    if protocol != ProtocolEdition::Ocpp201 {
        return;
    }

    let enabled = [
        ("SetVariableMonitoring", options.set_variable_monitoring),
        ("ClearVariableMonitoring", options.clear_variable_monitoring),
        ("GetMonitoringReport", options.get_monitoring_report),
        ("SetMonitoringBase", options.set_monitoring_base),
        ("SetMonitoringLevel", options.set_monitoring_level),
    ];

    for (action, option) in enabled {
        if option.enabled() {
            snapshot.capabilities.operations.push(SupportedOperation {
                operation: Operation::ProtocolAction {
                    protocol,
                    action: action.to_owned(),
                },
                parameters: Vec::new(),
            });
        }
    }
}
