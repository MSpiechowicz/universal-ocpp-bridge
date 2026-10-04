//! Reference-only station list updates and value-free native acknowledgements.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SEND_LOCAL_LIST_REFERENCE_SCHEMA_16: &str = "urn:uob:ocpp16:SendLocalListReference:1";
pub const LOCAL_AUTHORIZATION_ENTRIES_LIMIT_16: usize = 256;
pub const LOCAL_AUTHORIZATION_BYTES_LIMIT_16: usize = 64 * 1024;

#[must_use]
pub fn valid_local_list_reference_16(reference: &str) -> bool {
    reference.len() == 71
        && reference.starts_with("list16:")
        && reference.as_bytes()[7..].iter().all(u8::is_ascii_hexdigit)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum LocalListUpdateType16 {
    Full,
    Differential,
}

#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendLocalListReference16 {
    #[schemars(range(min = i32::MIN, max = i32::MAX))]
    #[schemars(extend("not" = serde_json::json!({"enum": [-1, 0]})))]
    pub list_version: i32,
    pub update_type: LocalListUpdateType16,
    #[schemars(
        length(min = 71, max = 71),
        regex(pattern = "^list16:[0-9a-fA-F]{64}$")
    )]
    pub update_reference: String,
}
impl SendLocalListReference16 {
    #[must_use]
    pub fn valid(&self) -> bool {
        !matches!(self.list_version, -1 | 0)
            && valid_local_list_reference_16(&self.update_reference)
    }
}

impl<'de> Deserialize<'de> for SendLocalListReference16 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Envelope {
            list_version: i32,
            update_type: LocalListUpdateType16,
            update_reference: String,
        }
        let envelope = Envelope::deserialize(deserializer)?;
        let request = Self {
            list_version: envelope.list_version,
            update_type: envelope.update_type,
            update_reference: envelope.update_reference,
        };
        if !request.valid() {
            return Err(serde::de::Error::custom(
                "invalid protected local list reference",
            ));
        }
        Ok(request)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum SendLocalListStatus16 {
    Accepted,
    Failed,
    NotSupported,
    VersionMismatch,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ClearCacheStatus16 {
    Accepted,
    Rejected,
}

/// No identities, parents, capabilities, contents or arbitrary native error text.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum LocalAuthorizationResult16 {
    GetLocalListVersion {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        list_version: i32,
    },
    SendLocalList {
        #[schemars(range(min = i32::MIN, max = i32::MAX))]
        list_version: i32,
        update_type: LocalListUpdateType16,
        status: SendLocalListStatus16,
    },
    ClearCache {
        status: ClearCacheStatus16,
    },
}
impl LocalAuthorizationResult16 {
    #[must_use]
    pub const fn accepted(&self) -> bool {
        match self {
            Self::GetLocalListVersion { .. } => true,
            Self::SendLocalList { status, .. } => matches!(status, SendLocalListStatus16::Accepted),
            Self::ClearCache { status } => matches!(status, ClearCacheStatus16::Accepted),
        }
    }
}
