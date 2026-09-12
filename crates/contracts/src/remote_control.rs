use serde::{Deserialize, Serialize};

/// Native protocol evidence is separate from physical transaction effects.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteControlEvidence {
    pub remote_start_id: Option<i32>,
    pub response_status: Option<String>,
    pub native_transaction_id: Option<String>,
}
