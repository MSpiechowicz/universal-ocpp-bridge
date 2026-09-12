//! Deliberately lossy, identity-free import of sanitized status observations.
//! Raw captures, snapshots, database rows and command/export envelopes are not this format.
use super::configuration::{DOCUMENT_LIMIT, validate_simulator};
use crate::scenario::{
    ConfiguredOcppVersion, ScenarioDefinition, SimulatorConfiguration, StepDefinition,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u16,
    kind: Kind,
    records: Vec<Record>,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    SanitizedCapture,
    SanitizedSnapshot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    // Ordinals are explicitly mapped to administrator-configured synthetic test peers.
    station_slot: usize,
    connector_slot: usize,
    status: Status,
}
#[derive(Clone, Copy, Deserialize)]
enum Status {
    Available,
    Unavailable,
    Faulted,
}
impl Status {
    const fn text(self) -> &'static str {
        match self {
            Self::Available => "Available",
            Self::Unavailable => "Unavailable",
            Self::Faulted => "Faulted",
        }
    }
}

pub(super) fn parse(
    input: &str,
    simulator: &SimulatorConfiguration,
    environment: &str,
) -> Result<ScenarioDefinition, &'static str> {
    if environment != "staging" {
        return Err("staging_import_required");
    }
    validate_simulator(simulator, environment)?;
    if input.len() > DOCUMENT_LIMIT {
        return Err("import_byte_limit");
    }
    let document: Document = serde_json::from_str(input).map_err(|_| "invalid_sanitized_import")?;
    if document.schema_version != 1 || document.records.is_empty() || document.records.len() > 32 {
        return Err("import_version_or_record_limit");
    }
    let mut resources = BTreeSet::new();
    let mut stations = BTreeSet::new();
    let mut steps = Vec::new();
    for record in document.records {
        if matches!(document.kind, Kind::SanitizedSnapshot)
            && !resources.insert((record.station_slot, record.connector_slot))
        {
            return Err("duplicate_snapshot_resource");
        }
        let station = simulator
            .stations
            .get(record.station_slot)
            .ok_or("unknown_import_station_slot")?;
        let payload = match station.ocpp_version {
            ConfiguredOcppVersion::V1_6 => {
                let connector = *station
                    .connector_ids()
                    .get(record.connector_slot)
                    .ok_or("unknown_import_connector_slot")?;
                json!({"connectorId": connector, "status": record.status.text(),
                    "errorCode": if matches!(record.status, Status::Faulted) { "OtherError" } else { "NoError" }})
            }
            ConfiguredOcppVersion::V2_0_1 => {
                let (evse, connector) = *station
                    .evse_connectors()
                    .get(record.connector_slot)
                    .ok_or("unknown_import_connector_slot")?;
                json!({"evseId": evse, "connectorId": connector, "connectorStatus": record.status.text(),
                    "timestamp": "2000-01-01T00:00:00Z"})
            }
        };
        if stations.insert(record.station_slot) {
            steps.push(step(&station.id, "connect", steps.len(), None)?);
            let boot = match station.ocpp_version {
                ConfiguredOcppVersion::V1_6 => {
                    json!({"chargePointVendor":"UOB", "chargePointModel":"Sanitized import"})
                }
                ConfiguredOcppVersion::V2_0_1 => {
                    json!({"reason":"PowerUp", "chargingStation":{"vendorName":"UOB", "model":"Sanitized import"}})
                }
            };
            steps.push(step(&station.id, "boot", steps.len(), Some(&boot))?);
        }
        steps.push(step(&station.id, "status", steps.len(), Some(&payload))?);
    }
    for slot in stations {
        steps.push(step(
            &simulator.stations[slot].id,
            "disconnect",
            steps.len(),
            None,
        )?);
    }
    Ok(ScenarioDefinition {
        schema_version: 1,
        seed: 0,
        steps,
    })
}

fn step(
    station: &str,
    action: &str,
    sequence: usize,
    payload: Option<&Value>,
) -> Result<StepDefinition, &'static str> {
    serde_json::from_value(json!({
        "id": format!("staging-import-{sequence}"), "station": station, "action": action,
        "timeout_ms": 1000, "payload": payload,
        "fixture_id": payload.as_ref().map(|_| "sanitized-status-v1"),
        "expect_response": if action == "status" { Some(json!({})) } else { None },
    }))
    .map_err(|_| "invalid_import_step")
}

/// Rebind every replay, including repeats of the same catalog entry, to fresh evidence IDs.
pub(super) fn reidentify(scenario: &mut ScenarioDefinition) -> String {
    let identity = format!("staging-import-{}", uuid::Uuid::new_v4().simple());
    for (index, step) in scenario.steps.iter_mut().enumerate() {
        step.id = format!("{identity}-{index}");
    }
    identity
}
