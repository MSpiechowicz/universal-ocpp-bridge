//! Reference-only station list updates and value-free native acknowledgements.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SEND_LOCAL_LIST_REFERENCE_SCHEMA_201: &str = "urn:uob:ocpp201:SendLocalListReference:1";
pub const LOCAL_AUTHORIZATION_ENTRIES_LIMIT_201: usize = 256;
pub const LOCAL_AUTHORIZATION_BYTES_LIMIT_201: usize = 64 * 1024;

#[must_use]
pub fn valid_local_list_reference_201(reference: &str) -> bool {
    reference.len() == 72
        && reference.starts_with("list201:")
        && reference.as_bytes()[8..].iter().all(u8::is_ascii_hexdigit)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum LocalListUpdateType201 {
    Full,
    Differential,
}

#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendLocalListReference201 {
    #[schemars(range(min = 1, max = i32::MAX))]
    pub version_number: i32,
    pub update_type: LocalListUpdateType201,
    #[schemars(
        length(min = 72, max = 72),
        regex(pattern = "^list201:[0-9a-fA-F]{64}$")
    )]
    pub update_reference: String,
}
impl SendLocalListReference201 {
    #[must_use]
    pub fn valid(&self) -> bool {
        self.version_number > 0 && valid_local_list_reference_201(&self.update_reference)
    }
}

impl<'de> Deserialize<'de> for SendLocalListReference201 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Envelope {
            version_number: i32,
            update_type: LocalListUpdateType201,
            update_reference: String,
        }
        let envelope = Envelope::deserialize(deserializer)?;
        let request = Self {
            version_number: envelope.version_number,
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
pub enum SendLocalListStatus201 {
    Accepted,
    Failed,
    VersionMismatch,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ClearCacheStatus201 {
    Accepted,
    Rejected,
}

/// No identities, parents, capabilities, contents or arbitrary native error text.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum LocalAuthorizationResult201 {
    GetLocalListVersion {
        #[schemars(range(min = 0, max = i32::MAX))]
        version_number: i32,
    },
    SendLocalList {
        #[schemars(range(min = 1, max = i32::MAX))]
        version_number: i32,
        update_type: LocalListUpdateType201,
        status: SendLocalListStatus201,
    },
    ClearCache {
        status: ClearCacheStatus201,
    },
}
impl LocalAuthorizationResult201 {
    #[must_use]
    pub const fn accepted(&self) -> bool {
        match self {
            Self::GetLocalListVersion { .. } => true,
            Self::SendLocalList { status, .. } => {
                matches!(status, SendLocalListStatus201::Accepted)
            }
            Self::ClearCache { status } => matches!(status, ClearCacheStatus201::Accepted),
        }
    }
}
