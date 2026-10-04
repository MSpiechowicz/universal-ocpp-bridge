//! The pinned native library's `CiString20` uses a 20-byte heapless buffer.
//! OCPP's limit is 20 Unicode characters, so identity calls use its correlated
//! generic Action API with independent string-bearing native wire models.
use ocpp_client::Action;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use super::NativeInfo;
use crate::{SimulatorAction, SimulatorClientError};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AuthorizationRequest {
    id_tag: String,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AuthorizationResponse {
    id_tag_info: NativeInfo,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StartRequest {
    connector_id: i64,
    id_tag: String,
    meter_start: i64,
    timestamp: String,
    #[serde(
        default,
        deserialize_with = "super::non_null",
        skip_serializing_if = "Option::is_none"
    )]
    reservation_id: Option<i64>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StartResponse {
    transaction_id: i64,
    id_tag_info: NativeInfo,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StopRequest {
    transaction_id: i64,
    meter_stop: i64,
    timestamp: String,
    #[serde(
        default,
        deserialize_with = "super::non_null",
        skip_serializing_if = "Option::is_none"
    )]
    id_tag: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::non_null",
        skip_serializing_if = "Option::is_none"
    )]
    reason: Option<ocpp_client::ocpp_types::v16::common::Reason>,
    #[serde(
        default,
        deserialize_with = "super::non_null",
        skip_serializing_if = "Option::is_none"
    )]
    transaction_data: Option<Vec<ocpp_client::ocpp_types::v16::common::TransactionDataItem>>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StopResponse {
    #[serde(
        default,
        deserialize_with = "super::non_null",
        skip_serializing_if = "Option::is_none"
    )]
    id_tag_info: Option<NativeInfo>,
}

struct Authorize;
impl Action for Authorize {
    const NAME: &'static str = "Authorize";
    type Request = AuthorizationRequest;
    type Response = AuthorizationResponse;
}
struct Start;
impl Action for Start {
    const NAME: &'static str = "StartTransaction";
    type Request = StartRequest;
    type Response = StartResponse;
}
struct Stop;
impl Action for Stop {
    const NAME: &'static str = "StopTransaction";
    type Request = StopRequest;
    type Response = StopResponse;
}

pub(crate) async fn call(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    action: SimulatorAction,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, SimulatorClientError> {
    if payload
        .get("idTag")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|token| token.chars().count() > 20)
    {
        return Err(error());
    }
    if matches!(
        action,
        SimulatorAction::StartTransaction | SimulatorAction::StopTransaction
    ) && !payload
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|timestamp| {
            time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
                .is_ok()
        })
    {
        return Err(error());
    }
    macro_rules! exchange {
        ($action:ty, $valid:expr) => {{
            let request =
                <$action as Action>::Request::deserialize(payload).map_err(|_| error())?;
            let response = client.call::<$action>(request).await.map_err(|_| error())?;
            if !($valid)(&response) {
                return Err(error());
            }
            serde_json::to_value(response).map_err(|_| error())
        }};
    }
    match action {
        SimulatorAction::Authorize => exchange!(Authorize, |response: &AuthorizationResponse| {
            super::model::valid_info(&response.id_tag_info)
        }),
        SimulatorAction::StartTransaction => {
            exchange!(Start, |response: &StartResponse| super::model::valid_info(
                &response.id_tag_info
            ))
        }
        SimulatorAction::StopTransaction => exchange!(Stop, |response: &StopResponse| response
            .id_tag_info
            .as_ref()
            .is_none_or(super::model::valid_info)),
        _ => Err(error()),
    }
}

fn error() -> SimulatorClientError {
    SimulatorClientError::Protocol("native identity exchange failed".to_owned())
}

impl Drop for AuthorizationRequest {
    fn drop(&mut self) {
        self.id_tag.zeroize();
    }
}
impl Drop for StartRequest {
    fn drop(&mut self) {
        self.id_tag.zeroize();
    }
}
impl Drop for StopRequest {
    fn drop(&mut self) {
        self.id_tag.zeroize();
    }
}
