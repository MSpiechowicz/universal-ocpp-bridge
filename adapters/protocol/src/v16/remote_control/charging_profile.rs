use rust_ocpp::v1_6::{
    messages::{
        clear_charging_profile::ClearChargingProfileResponse,
        set_charging_profile::SetChargingProfileResponse,
    },
    types::{ChargingProfileStatus, ClearChargingProfileStatus},
};
use serde::Deserialize;
use serde_json::Value;
use uob_application::CommandDispatchOutcome;
use uob_contracts::{
    ChargingProfileResult16, ClearChargingProfileStatus16, CommandErrorCode,
    PrivilegedOcppOperation, ResourceRef, SetChargingProfileStatus16, StationSnapshot,
    TransactionState,
};

use crate::command_registry::charging_profile16::{self as registry, Request};

pub(super) fn prepare(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
    snapshot: &StationSnapshot,
) -> Result<Request, CommandErrorCode> {
    let request = registry::validate(resource, operation)?;
    if let Request::Set(request) = &request
        && let Some(native_id) = request.cs_charging_profiles.transaction_id
    {
        // A second non-ended local row or reused native identity is ambiguous.
        let mut current = None;
        let mut native_matches = 0;
        for transaction in &snapshot.transactions {
            if transaction.ended_at.is_some() || transaction.state == TransactionState::Ended {
                continue;
            }
            if transaction.resource == *resource {
                if current.is_some() {
                    return Err(CommandErrorCode::InvalidParameters);
                }
                current = Some(transaction);
            }
            if transaction
                .ocpp16
                .as_ref()
                .is_some_and(|evidence| evidence.transaction_id == native_id)
            {
                native_matches += 1;
                if native_matches > 1 {
                    return Err(CommandErrorCode::InvalidParameters);
                }
            }
        }

        let transaction = current.ok_or(CommandErrorCode::InvalidParameters)?;
        if !matches!(
            transaction.state,
            TransactionState::Pending | TransactionState::Active | TransactionState::Suspended
        ) {
            return Err(CommandErrorCode::InvalidParameters);
        }

        let evidence = transaction
            .ocpp16
            .as_ref()
            .ok_or(CommandErrorCode::InvalidParameters)?;
        if evidence.transaction_id != native_id || evidence.stop_message_id.is_some() {
            return Err(CommandErrorCode::InvalidParameters);
        }
    }
    Ok(request)
}

pub(super) fn response(request: Request, payload: &Value) -> CommandDispatchOutcome {
    let evidence = match request {
        Request::Set(request) if registry::valid_schema(1, payload) => {
            let Ok(response) = SetChargingProfileResponse::deserialize(payload) else {
                return super::mapping::uncertain();
            };
            let status = match response.status {
                ChargingProfileStatus::Accepted => SetChargingProfileStatus16::Accepted,
                ChargingProfileStatus::Rejected => SetChargingProfileStatus16::Rejected,
                ChargingProfileStatus::NotSupported => SetChargingProfileStatus16::NotSupported,
            };
            ChargingProfileResult16::SetChargingProfile { request, status }
        }
        Request::Clear(request) if registry::valid_schema(3, payload) => {
            let Ok(response) = ClearChargingProfileResponse::deserialize(payload) else {
                return super::mapping::uncertain();
            };
            let status = match response.status {
                ClearChargingProfileStatus::Accepted => ClearChargingProfileStatus16::Accepted,
                ClearChargingProfileStatus::Unknown => ClearChargingProfileStatus16::Unknown,
            };
            ChargingProfileResult16::ClearChargingProfile { request, status }
        }
        _ => return super::mapping::uncertain(),
    };
    CommandDispatchOutcome::ChargingProfileResponse16(evidence)
}
