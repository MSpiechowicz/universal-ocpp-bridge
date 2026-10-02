//! Independently authored software state, not a production validator or power model.
#[path = "peer/regressions.rs"]
#[cfg(test)]
mod regressions;
#[path = "peer/state.rs"]
mod state;
#[path = "peer/validation.rs"]
mod validation;
#[path = "peer/wire.rs"]
pub mod wire;

use serde_json::{Value, json};
use std::{collections::BTreeMap, io, path::PathBuf, time::Duration};
pub use wire::Socket;

pub const CAPACITY: usize = 128;
pub const FRAME_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Result(Value),
    Error(&'static str),
}

#[derive(Default)]
pub struct Controls {
    pub reject_next: bool,
    pub delay: Duration,
    pub disconnect_after_apply: bool,
}

#[derive(Default)]
pub struct Counters {
    pub set: u64,
    pub clear: u64,
    pub get_variables: u64,
}

pub struct Peer {
    path: PathBuf,
    profiles: Vec<Value>,
    phases: BTreeMap<u32, bool>,
    transactions: BTreeMap<u32, String>,
    pub controls: Controls,
    pub counters: Counters,
}

impl Peer {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let profiles = state::load(&path)?;
        Ok(Self {
            path,
            profiles,
            phases: BTreeMap::new(),
            transactions: BTreeMap::new(),
            controls: Controls::default(),
            counters: Counters::default(),
        })
    }

    pub fn profiles(&self) -> &[Value] {
        &self.profiles
    }

    pub fn set_phase(&mut self, evse: u32, supported: bool) {
        assert!(evse > 0 && i32::try_from(evse).is_ok());
        assert!(self.phases.contains_key(&evse) || self.phases.len() < CAPACITY);
        self.phases.insert(evse, supported);
    }

    pub fn set_transaction(&mut self, evse: u32, transaction: Option<&str>) {
        assert!(evse > 0 && i32::try_from(evse).is_ok());
        if let Some(id) = transaction {
            assert!(!id.is_empty() && id.chars().count() <= 36);
            assert!(self.transactions.contains_key(&evse) || self.transactions.len() < CAPACITY);
            self.transactions.insert(evse, id.to_owned());
        } else {
            self.transactions.remove(&evse);
        }
    }

    pub fn seed_external(&mut self, payload: Value) -> io::Result<()> {
        if payload["chargingProfile"]["chargingProfilePurpose"]
            != "ChargingStationExternalConstraints"
            || !validation::profile(&payload, &self.phases, &self.transactions, true)
        {
            return Err(io::Error::other("invalid independent external profile"));
        }
        self.install(payload)
    }

    pub fn apply(&mut self, action: &str, payload: &Value) -> io::Result<Reply> {
        match action {
            "SetChargingProfile" => {
                self.counters.set += 1;
                let denied = std::mem::take(&mut self.controls.reject_next);
                if denied
                    || !validation::profile(payload, &self.phases, &self.transactions, false)
                    || self
                        .profiles
                        .iter()
                        .any(|old| validation::conflicts(old, payload))
                    || (self.profiles.len() == CAPACITY
                        && !self
                            .profiles
                            .iter()
                            .any(|old| validation::same_id(old, payload)))
                {
                    return Ok(status("Rejected"));
                }
                self.install(payload.clone())?;
                Ok(status("Accepted"))
            }
            "ClearChargingProfile" => {
                self.counters.clear += 1;
                if !validation::clear(payload) {
                    return Ok(Reply::Error("FormationViolation"));
                }
                if std::mem::take(&mut self.controls.reject_next) {
                    return Ok(status("Unknown"));
                }
                let profiles: Vec<_> = self
                    .profiles
                    .iter()
                    .filter(|old| !validation::matches_clear(old, payload))
                    .cloned()
                    .collect();
                if profiles.len() == self.profiles.len() {
                    return Ok(status("Unknown"));
                }
                state::persist(&self.path, &profiles)?;
                self.profiles = profiles;
                Ok(status("Accepted"))
            }
            "GetVariables" => {
                self.counters.get_variables += 1;
                Ok(wire::variables(payload, &self.phases))
            }
            _ => Ok(Reply::Error("NotImplemented")),
        }
    }

    fn install(&mut self, payload: Value) -> io::Result<()> {
        let mut profiles: Vec<_> = self
            .profiles
            .iter()
            .filter(|old| !validation::same_id(old, &payload))
            .cloned()
            .collect();
        if profiles.len() >= CAPACITY {
            return Err(io::Error::other("independent profile capacity"));
        }
        profiles.push(payload);
        state::persist(&self.path, &profiles)?;
        self.profiles = profiles;
        Ok(())
    }
}

fn status(value: &'static str) -> Reply {
    Reply::Result(json!({"status":value}))
}
