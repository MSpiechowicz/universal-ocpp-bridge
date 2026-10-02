//! Narrow browser-fixture profile state; no complex native composer or enforcement simulation.
use super::{Result, state::State};
use serde_json::{Value, json};

pub(super) fn install(state: &mut State, payload: &Value) {
    let id = &payload["chargingProfile"]["id"];
    state
        .profiles201
        .retain(|profile| &profile["chargingProfile"]["id"] != id);
    state.profiles201.push(payload.clone());
}
pub(super) fn clear(state: &mut State, payload: &Value) -> Result<Value> {
    let fields = payload.as_object().ok_or("invalid clear request")?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "chargingProfileId" | "chargingProfileCriteria"
        )
    }) {
        return Err("invalid clear request");
    }
    let id = payload
        .get("chargingProfileId")
        .map(|value| value.as_i64().ok_or("invalid clear id"))
        .transpose()?;
    let criteria = payload.get("chargingProfileCriteria");
    if id.is_some() == criteria.is_some() {
        return Err("invalid clear selectors");
    }
    if let Some(criteria) = criteria {
        let fields = criteria.as_object().ok_or("invalid clear criteria")?;
        if fields.is_empty()
            || fields.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "evseId" | "chargingProfilePurpose" | "stackLevel"
                )
            })
        {
            return Err("invalid clear criteria");
        }
    }
    let before = state.profiles201.len();
    state.profiles201.retain(|profile| {
        let native = &profile["chargingProfile"];
        if native["chargingProfilePurpose"] == "ChargingStationExternalConstraints" {
            return true;
        }
        if let Some(id) = id {
            return native["id"].as_i64() != Some(id);
        }
        let criteria = criteria.expect("validated criteria");
        !criteria
            .get("evseId")
            .is_none_or(|evse| evse == &profile["evseId"])
            || !criteria
                .get("chargingProfilePurpose")
                .is_none_or(|purpose| purpose == &native["chargingProfilePurpose"])
            || !criteria
                .get("stackLevel")
                .is_none_or(|level| level == &native["stackLevel"])
    });
    Ok(json!({"status": if state.profiles201.len() == before { "Unknown" } else { "Accepted" }}))
}
