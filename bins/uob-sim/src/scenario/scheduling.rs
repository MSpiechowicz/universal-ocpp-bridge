use std::collections::{HashMap, HashSet};

use super::{
    FailureCategory, FaultDefinition, FaultKind, RunFailure, ScenarioDefinition,
    SimulatorConfiguration, StepDefinition,
};

#[derive(Clone)]
pub(super) struct StepWork {
    pub index: usize,
    pub step: StepDefinition,
}

pub(super) fn validate_and_group_steps(
    configuration: &SimulatorConfiguration,
    scenario: &ScenarioDefinition,
) -> Result<HashMap<String, Vec<StepWork>>, (RunFailure, u64)> {
    let stations: HashMap<_, _> = configuration
        .stations
        .iter()
        .map(|station| (station.id.as_str(), station))
        .collect();
    let station_ids: HashSet<_> = stations.keys().copied().collect();
    if scenario
        .steps
        .iter()
        .any(|step| !station_ids.contains(step.station.as_str()))
    {
        return Err((
            RunFailure::new(
                FailureCategory::Setup,
                "unknown_station",
                "scenario references a station not present in the configuration",
            ),
            1,
        ));
    }

    let mut grouped: HashMap<String, Vec<StepWork>> = HashMap::new();
    for (index, step) in scenario.steps.iter().cloned().enumerate() {
        grouped
            .entry(step.station.clone())
            .or_default()
            .push(StepWork { index, step });
    }
    for (station_id, steps) in &grouped {
        let station = stations[station_id.as_str()];
        let capacity = station.step_capacity;
        if steps.len() > capacity {
            return Err((
                RunFailure::new(
                    FailureCategory::Setup,
                    "station_step_capacity_exceeded",
                    "scenario schedules more station work than its bounded queue permits",
                ),
                (steps.len() - capacity) as u64,
            ));
        }
        if station.command_capacity < 2
            && steps.iter().any(|work| {
                work.step
                    .fault
                    .as_ref()
                    .is_some_and(|fault| fault.kind == FaultKind::OutOfOrderResponse)
            })
        {
            return Err((
                RunFailure::new(
                    FailureCategory::Setup,
                    "outstanding_capacity_insufficient",
                    "out-of-order response control requires two outstanding exchanges",
                ),
                1,
            ));
        }
    }
    Ok(grouped)
}

pub(super) fn deterministic_jitter(seed: u64, station: &str, step: &str, maximum: u64) -> u64 {
    if maximum == 0 {
        return 0;
    }
    deterministic_value(seed, station, step, 0x6a09_e667_f3bc_c909) % maximum.saturating_add(1)
}

pub(super) fn fault_selected(
    seed: u64,
    station: &str,
    step: &str,
    fault: &FaultDefinition,
) -> bool {
    deterministic_value(seed, station, step, 0xbb67_ae85_84ca_a73b) % 100
        < u64::from(fault.probability_percent)
}

fn deterministic_value(seed: u64, station: &str, step: &str, salt: u64) -> u64 {
    let mut value = seed ^ salt;
    for byte in station.bytes().chain([0xff]).chain(step.bytes()) {
        value ^= u64::from(byte);
        value = value.wrapping_mul(0x0000_0100_0000_01b3);
    }
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
