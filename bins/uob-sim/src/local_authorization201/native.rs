use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroize;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum TokenType {
    Central,
    #[serde(rename = "eMAID")]
    Emaid,
    #[serde(rename = "ISO14443")]
    Iso14443,
    #[serde(rename = "ISO15693")]
    Iso15693,
    KeyCode,
    Local,
    MacAddress,
    NoAuthorization,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum AuthorizationStatus {
    Accepted,
    Blocked,
    ConcurrentTx,
    Expired,
    Invalid,
    NoCredit,
    #[serde(rename = "NotAllowedTypeEVSE")]
    NotAllowedTypeEvse,
    NotAtThisLocation,
    NotAtThisTime,
    Unknown,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum UpdateType {
    Full,
    Differential,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdToken {
    pub id_token: String,
    #[serde(rename = "type")]
    pub token_type: TokenType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_info: Option<Vec<AdditionalInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<Value>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdditionalInfo {
    pub additional_id_token: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<Value>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdTokenInfo {
    pub status: AuthorizationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_expiry_date_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charging_priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language2: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evse_id: Option<Vec<i32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id_token: Option<IdToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personal_message: Option<MessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<Value>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MessageContent {
    pub format: MessageFormat,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(
        default,
        rename = "customData",
        skip_serializing_if = "Option::is_none"
    )]
    pub custom_data: Option<Value>,
}
#[derive(Clone, Deserialize, Serialize)]
pub enum MessageFormat {
    #[serde(rename = "ASCII")]
    Ascii,
    #[serde(rename = "HTML")]
    Html,
    #[serde(rename = "URI")]
    Uri,
    #[serde(rename = "UTF8")]
    Utf8,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    pub id_token: IdToken,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token_info: Option<IdTokenInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<Value>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Update {
    pub version_number: i32,
    pub update_type: UpdateType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_authorization_list: Option<Vec<Entry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<Value>,
}

pub(super) fn valid_date(date: &str) -> bool {
    let fraction = date
        .split_once('.')
        .map(|(_, rest)| rest.chars().take_while(char::is_ascii_digit).count());
    fraction.is_none_or(|digits| (1..=3).contains(&digits))
        && OffsetDateTime::parse(date, &Rfc3339).is_ok()
}

pub(super) fn allowed(info: &IdTokenInfo, evse: i32, now: OffsetDateTime) -> bool {
    info.status == AuthorizationStatus::Accepted
        && info.evse_id.as_ref().is_none_or(|ids| ids.contains(&evse))
        && info.cache_expiry_date_time.as_ref().is_none_or(|date| {
            OffsetDateTime::parse(date, &Rfc3339).is_ok_and(|expiry| expiry > now)
        })
}

pub(super) fn wipe(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(wipe),
        Value::Object(fields) => {
            for (mut key, mut value) in std::mem::take(fields) {
                key.zeroize();
                wipe(&mut value);
            }
        }
        _ => {}
    }
}
impl Drop for IdToken {
    fn drop(&mut self) {
        self.id_token.zeroize();
        if let Some(value) = &mut self.custom_data {
            wipe(value);
        }
    }
}
impl Drop for AdditionalInfo {
    fn drop(&mut self) {
        self.additional_id_token.zeroize();
        self.kind.zeroize();
        if let Some(value) = &mut self.custom_data {
            wipe(value);
        }
    }
}
impl Drop for MessageContent {
    fn drop(&mut self) {
        self.content.zeroize();
        self.language.zeroize();
        if let Some(value) = &mut self.custom_data {
            wipe(value);
        }
    }
}
impl Drop for IdTokenInfo {
    fn drop(&mut self) {
        self.language1.zeroize();
        self.language2.zeroize();
        self.cache_expiry_date_time.zeroize();
        if let Some(value) = &mut self.custom_data {
            wipe(value);
        }
    }
}
impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(value) = &mut self.custom_data {
            wipe(value);
        }
    }
}
impl Drop for Update {
    fn drop(&mut self) {
        if let Some(value) = &mut self.custom_data {
            wipe(value);
        }
    }
}

pub(super) struct PrivateJson(pub Value);
impl Drop for PrivateJson {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}
