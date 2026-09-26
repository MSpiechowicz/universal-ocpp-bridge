//! Transaction-bound `TxProfile` for Pending or Active sessions; acceptance is not physical charging evidence.
use rust_ocpp::v1_6::messages::set_charging_profile::{
    SetChargingProfileRequest, SetChargingProfileResponse,
};
use serde_json::{Value, json};
use uob_contracts::{
    ChargingLimit, Command, CommandErrorCode, NativeProtocolReference, StationSnapshot,
    TransactionState,
};
pub(super) fn prepare(
    command: &Command<Value>,
    snapshot: &StationSnapshot,
    limit: &ChargingLimit,
) -> Result<Value, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    let Some(NativeProtocolReference::Ocpp16 { connector_id }) =
        command.resource.native_protocol_reference
    else {
        return Err(invalid);
    };
    if command.resource == snapshot.station || connector_id == 0 || connector_id > i32::MAX as u32 {
        return Err(invalid);
    }
    let transaction = snapshot
        .transactions
        .iter()
        .find(|tx| {
            tx.resource == command.resource
                && matches!(
                    tx.state,
                    TransactionState::Pending | TransactionState::Active
                )
                && tx.ocpp16.is_some()
        })
        .ok_or(invalid)?;
    let native_id = transaction.ocpp16.as_ref().ok_or(invalid)?.transaction_id;
    if native_id <= 0 {
        return Err(invalid);
    }
    let (unit, amount) = super::super::super::remote_constraints::profile_quantity(limit)?;
    let payload = json!({
        "connectorId": connector_id,
        "csChargingProfiles": {
            "chargingProfileId": native_id,
            "transactionId": native_id,
            "stackLevel": 0,
            "chargingProfilePurpose": "TxProfile",
            "chargingProfileKind": "Relative",
            "chargingSchedule": {
                "chargingRateUnit": unit,
                "chargingSchedulePeriod": [{
                    "startPeriod": 0,
                    "limit": amount,
                    "numberPhases": limit.phases,
                }]
            }
        }
    });
    let request: SetChargingProfileRequest =
        serde_json::from_value(payload).map_err(|_| invalid)?;
    serde_json::to_value(request).map_err(|_| invalid)
}

pub(super) fn valid_response(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.len() == 1 && object.contains_key("status"))
        && serde_json::from_value::<SetChargingProfileResponse>(value.clone()).is_ok()
}
