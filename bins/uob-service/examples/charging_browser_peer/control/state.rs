//! Accepted central-system commands and active station transaction state.
use super::{Edition, Result};
use serde_json::{Value, json};
#[derive(Clone, Default)]
pub(super) struct Counts {
    pub(super) start: u8,
    pub(super) stop: u8,
    pub(super) limit: u8,
    pub(super) availability: u8,
    pub(super) started: u8,
    pub(super) ended: u8,
    pub(super) availability_observed: u8,
}

#[derive(Clone)]
pub(super) struct Start16 {
    pub(super) token: String,
    pub(super) connector: u64,
}
#[derive(Clone)]
pub(super) struct Start201 {
    pub(super) token: Value,
    pub(super) remote_id: i64,
    pub(super) evse: u64,
}
#[derive(Clone)]
pub(super) struct Availability16 {
    pub(super) connector: u64,
    pub(super) operative: bool,
}
#[derive(Clone)]
pub(super) struct Availability201 {
    pub(super) evse: Option<u64>,
    pub(super) operative: bool,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) counts: Counts,
    pub(super) prepared: bool,
    pub(super) seed_transaction: Option<i64>,
    pub(super) start16: Option<Start16>,
    pub(super) start201: Option<Start201>,
    pub(super) active16: Option<i64>,
    pub(super) active201: Option<String>,
    pub(super) stop_requested: bool,
    pub(super) availability16: Option<Availability16>,
    pub(super) availability201: Option<Availability201>,
    pub(super) next_id: u32,
}

impl State {
    pub(super) fn accept(
        &mut self,
        edition: Edition,
        action: &str,
        payload: &Value,
    ) -> Result<Value> {
        match (edition, action) {
            (Edition::Alpha, "RemoteStartTransaction") => {
                let token = payload["idTag"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 20)
                    .ok_or("invalid remote start token")?;
                let connector = match payload.get("connectorId") {
                    None => 1,
                    Some(value) => value.as_u64().ok_or("invalid remote start connector")?,
                };
                self.counts.start = self.counts.start.saturating_add(1);
                if connector != 1 || self.active16.is_some() || self.seed_transaction.is_some() {
                    return Ok(json!({"status":"Rejected"}));
                }
                let incoming = Start16 {
                    token: token.to_owned(),
                    connector,
                };
                if self.start16.as_ref().is_some_and(|existing| {
                    existing.token != incoming.token || existing.connector != incoming.connector
                }) {
                    return Ok(json!({"status":"Rejected"}));
                }
                self.start16 = Some(incoming);
                Ok(json!({"status":"Accepted"}))
            }
            (Edition::Bravo, "RequestStartTransaction") => {
                let token = payload
                    .get("idToken")
                    .filter(|v| {
                        v["idToken"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty() && s.len() <= 36)
                            && v["type"].is_string()
                    })
                    .ok_or("invalid remote start identity")?;
                let remote_id = payload["remoteStartId"]
                    .as_i64()
                    .filter(|id| *id > 0)
                    .ok_or("invalid remote start id")?;
                let evse = match payload.get("evseId") {
                    None => 1,
                    Some(value) => value.as_u64().ok_or("invalid remote start evse")?,
                };
                self.counts.start = self.counts.start.saturating_add(1);
                if evse != 1 || self.active201.is_some() || !self.prepared {
                    return Ok(json!({"status":"Rejected"}));
                }
                let incoming = Start201 {
                    token: token.clone(),
                    remote_id,
                    evse,
                };
                if self.start201.as_ref().is_some_and(|existing| {
                    existing.token != incoming.token
                        || existing.remote_id != incoming.remote_id
                        || existing.evse != incoming.evse
                }) {
                    return Ok(json!({"status":"Rejected"}));
                }
                self.start201 = Some(incoming);
                Ok(json!({"status":"Accepted"}))
            }
            (Edition::Alpha, "RemoteStopTransaction") => {
                let id = payload["transactionId"]
                    .as_i64()
                    .ok_or("invalid remote stop id")?;
                self.counts.stop = self.counts.stop.saturating_add(1);
                if self.active16 != Some(id) {
                    return Ok(json!({"status":"Rejected"}));
                }
                self.stop_requested = true;
                Ok(json!({"status":"Accepted"}))
            }
            (Edition::Bravo, "RequestStopTransaction") => {
                let id = payload["transactionId"]
                    .as_str()
                    .ok_or("invalid remote stop id")?;
                self.counts.stop = self.counts.stop.saturating_add(1);
                if self.active201.as_deref() != Some(id) {
                    return Ok(json!({"status":"Rejected"}));
                }
                self.stop_requested = true;
                Ok(json!({"status":"Accepted"}))
            }
            (_, "SetChargingProfile") => {
                let valid = valid_profile(edition, payload, self);
                self.counts.limit = self.counts.limit.saturating_add(1);
                // A protocol acceptance is not a physical meter/limit observation.
                Ok(json!({"status":if valid { "Accepted" } else { "Rejected" }}))
            }
            (Edition::Alpha, "ChangeAvailability") => self.accept_availability_alpha(payload),
            (Edition::Bravo, "ChangeAvailability") => self.accept_availability_bravo(payload),
            _ => Err("unexpected server action"),
        }
    }

    fn accept_availability_alpha(&mut self, payload: &Value) -> Result<Value> {
        let connector = payload["connectorId"]
            .as_u64()
            .filter(|id| *id <= 1)
            .ok_or("invalid availability connector")?;
        let operative = match payload["type"].as_str() {
            Some("Operative") => true,
            Some("Inoperative") => false,
            _ => return Err("invalid availability type"),
        };
        self.counts.availability = self.counts.availability.saturating_add(1);
        self.availability16 = Some(Availability16 {
            connector,
            operative,
        });
        Ok(json!({"status":"Accepted"}))
    }

    fn accept_availability_bravo(&mut self, payload: &Value) -> Result<Value> {
        let operative = match payload["operationalStatus"].as_str() {
            Some("Operative") => true,
            Some("Inoperative") => false,
            _ => return Err("invalid availability type"),
        };
        let (evse, connector) = match payload.get("evse") {
            None => (None, None),
            Some(value) => {
                let evse = value["id"].as_u64().ok_or("invalid availability evse")?;
                let connector = value
                    .get("connectorId")
                    .map(|v| v.as_u64().ok_or("invalid availability connector"))
                    .transpose()?;
                (Some(evse), connector)
            }
        };
        if evse.is_some_and(|id| !(1..=2).contains(&id)) || connector.is_some_and(|id| id != 1) {
            return Err("invalid availability resource");
        }
        self.counts.availability = self.counts.availability.saturating_add(1);
        self.availability201 = Some(Availability201 { evse, operative });
        Ok(json!({"status":"Accepted"}))
    }
}

fn valid_profile(edition: Edition, payload: &Value, state: &State) -> bool {
    let profile = match edition {
        Edition::Alpha => {
            let Some(active) = state.active16 else {
                return false;
            };
            if payload["connectorId"].as_u64() != Some(1)
                || payload["csChargingProfiles"]["transactionId"].as_i64() != Some(active)
                || payload["csChargingProfiles"]["chargingProfileId"].as_i64() != Some(active)
            {
                return false;
            }
            &payload["csChargingProfiles"]
        }
        Edition::Bravo => {
            let Some(active) = state.active201.as_deref() else {
                return false;
            };
            if payload["evseId"].as_u64() != Some(1)
                || payload["chargingProfile"]["transactionId"].as_str() != Some(active)
            {
                return false;
            }
            &payload["chargingProfile"]
        }
    };
    if profile["chargingProfilePurpose"] != "TxProfile"
        || profile["chargingProfileKind"] != "Relative"
        || profile["stackLevel"].as_i64() != Some(0)
    {
        return false;
    }
    let schedule = match edition {
        Edition::Alpha => &profile["chargingSchedule"],
        Edition::Bravo => {
            let Some(id) = profile["id"].as_i64().filter(|id| *id > 0) else {
                return false;
            };
            let Some(schedules) = profile["chargingSchedule"].as_array() else {
                return false;
            };
            if schedules.len() != 1 || schedules[0]["id"].as_i64() != Some(id) {
                return false;
            }
            &schedules[0]
        }
    };
    let Some(periods) = schedule["chargingSchedulePeriod"].as_array() else {
        return false;
    };
    periods.len() == 1
        && schedule["chargingRateUnit"] == "A"
        && periods[0]["startPeriod"].as_u64() == Some(0)
        && periods[0]["limit"].as_f64() == Some(16.0)
        && periods[0]["numberPhases"].as_u64() == Some(1)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_matching_active_transaction_profile_and_quantity() {
        let mut state = State {
            active16: Some(41),
            active201: Some("native-tx".into()),
            ..State::default()
        };
        let mut alpha = json!({"connectorId":1,"csChargingProfiles":{
            "chargingProfileId":41,"transactionId":41,"stackLevel":0,
            "chargingProfilePurpose":"TxProfile","chargingProfileKind":"Relative",
            "chargingSchedule":{"chargingRateUnit":"A","chargingSchedulePeriod":[
                {"startPeriod":0,"limit":16,"numberPhases":1}]}}});
        let mut bravo = json!({"evseId":1,"chargingProfile":{
            "id":7,"transactionId":"native-tx","stackLevel":0,
            "chargingProfilePurpose":"TxProfile","chargingProfileKind":"Relative",
            "chargingSchedule":[{"id":7,"chargingRateUnit":"A","chargingSchedulePeriod":[
                {"startPeriod":0,"limit":16,"numberPhases":1}]}]}});
        assert!(valid_profile(Edition::Alpha, &alpha, &state));
        assert!(valid_profile(Edition::Bravo, &bravo, &state));

        alpha["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"] =
            json!(15);
        bravo["chargingProfile"]["chargingSchedule"][0]["chargingRateUnit"] = json!("W");
        assert!(!valid_profile(Edition::Alpha, &alpha, &state));
        assert!(!valid_profile(Edition::Bravo, &bravo, &state));
        alpha["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["limit"] =
            json!(16);
        bravo["chargingProfile"]["chargingSchedule"][0]["chargingRateUnit"] = json!("A");
        alpha["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["numberPhases"] =
            json!(3);
        bravo["chargingProfile"]["transactionId"] = json!("wrong-tx");
        assert!(!valid_profile(Edition::Alpha, &alpha, &state));
        assert!(!valid_profile(Edition::Bravo, &bravo, &state));
        alpha["csChargingProfiles"]["chargingSchedule"]["chargingSchedulePeriod"][0]["numberPhases"] =
            json!(1);
        alpha["csChargingProfiles"]["transactionId"] = json!(42);
        assert!(!valid_profile(Edition::Alpha, &alpha, &state));
        alpha["csChargingProfiles"]["transactionId"] = json!(41);
        assert!(valid_profile(Edition::Alpha, &alpha, &state));
        state.active16 = None;
        assert!(!valid_profile(Edition::Alpha, &alpha, &state));
    }
}
