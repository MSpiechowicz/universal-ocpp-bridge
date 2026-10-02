//! Bounded independently authored software peer. ACKs and persisted profiles do not prove hardware enforcement.
use super::support::{Socket, receive, send};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

pub struct Peer {
    path: PathBuf,
    profiles: Vec<Value>,
}
impl Peer {
    pub fn open(path: PathBuf) -> Self {
        let profiles = if path.exists() {
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap()
        } else {
            vec![]
        };
        Self { path, profiles }
    }
    pub fn profiles(&self) -> &[Value] {
        &self.profiles
    }
    pub fn apply(&mut self, action: &str, payload: &Value) -> &'static str {
        let status = match action {
            "SetChargingProfile" => {
                let profile = &payload["csChargingProfiles"];
                self.profiles.retain(|old| {
                    old["csChargingProfiles"]["chargingProfileId"] != profile["chargingProfileId"]
                        && !(old["connectorId"] == payload["connectorId"]
                            && old["csChargingProfiles"]["chargingProfilePurpose"]
                                == profile["chargingProfilePurpose"]
                            && old["csChargingProfiles"]["stackLevel"] == profile["stackLevel"])
                });
                assert!(
                    self.profiles.len() < 16,
                    "bounded independent peer profile capacity"
                );
                self.profiles.push(payload.clone());
                "Accepted"
            }
            "ClearChargingProfile" => {
                let before = self.profiles.len();
                self.profiles
                    .retain(|profile| !matches_clear(profile, payload));
                if self.profiles.len() == before {
                    "Unknown"
                } else {
                    "Accepted"
                }
            }
            _ => panic!("independent peer does not implement this action"),
        };
        let temp = self.path.with_extension("new");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .unwrap();
        file.write_all(&serde_json::to_vec(&self.profiles).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        fs::rename(temp, &self.path).unwrap();
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600)).unwrap();
        status
    }
    pub async fn serve_one(&mut self, socket: &mut Socket) -> Value {
        let call = receive(socket).await;
        assert_eq!(call[0], 2);
        let status = self.apply(call[2].as_str().unwrap(), &call[3]);
        send(socket, json!([3,call[1],{"status":status}])).await;
        call
    }
}
fn matches_clear(profile: &Value, selectors: &Value) -> bool {
    if let Some(id) = selectors.get("id") {
        return profile["csChargingProfiles"]["chargingProfileId"] == *id;
    }
    selectors
        .get("connectorId")
        .is_none_or(|id| profile["connectorId"] == *id)
        && selectors
            .get("chargingProfilePurpose")
            .is_none_or(|purpose| {
                profile["csChargingProfiles"]["chargingProfilePurpose"] == *purpose
            })
        && selectors
            .get("stackLevel")
            .is_none_or(|stack| profile["csChargingProfiles"]["stackLevel"] == *stack)
}
