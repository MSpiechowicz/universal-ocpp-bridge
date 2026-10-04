use super::super::{IdTokenInfo, validation};
use super::OwnedSink;
use serde::Deserialize;
use serde_json::{Value, json};

impl OwnedSink {
    /// Validate the original correlated native metadata before the SDK can
    /// discard unknown fields or normalize an explicit null into omission.
    pub(super) fn authorization_reply(&mut self, frame: &[Value]) -> Option<Value> {
        let id = frame.get(1)?.as_str()?;
        if frame.first() == Some(&json!(4)) {
            self.authorizations.remove(id);
            return None;
        }
        if frame.len() != 3 || frame.first() != Some(&json!(3)) {
            return None;
        }
        let (token, sent) = self.authorizations.remove(id)?;
        if sent.elapsed() >= self.timeout {
            return None;
        }
        let local = {
            let state = self.state.lock().expect("native state lock");
            if state.socket_generation != self.generation
                || !state.socket_connected
                || !state.registered
            {
                return None;
            }
            state.local.clone()
        }?;
        let info = frame[2].get("idTokenInfo")?;
        if !validation::valid_info(info) {
            return Some(json!([
                4,
                id,
                "FormationViolation",
                "native authorization information invalid",
                {}
            ]));
        }
        let Ok(info) = IdTokenInfo::deserialize(info) else {
            return Some(json!([
                4,
                id,
                "FormationViolation",
                "native authorization information invalid",
                {}
            ]));
        };
        if local.observe_central(token, info).is_err() {
            return Some(json!([
                4,
                id,
                "InternalError",
                "native private cache unavailable",
                {}
            ]));
        }
        None
    }
}
