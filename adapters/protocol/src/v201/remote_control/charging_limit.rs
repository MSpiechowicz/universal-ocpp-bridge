//! Transaction-bound `TxProfile` for Pending or Active EVSE sessions; acceptance only confirms profile receipt.
use rust_ocpp::v2_0_1::messages::set_charging_profile::{
    SetChargingProfileRequest, SetChargingProfileResponse,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uob_contracts::{
    ChargingLimit, Command, CommandErrorCode, NativeProtocolReference, ProtocolEdition,
    StationSnapshot, TransactionState,
};

pub(super) fn prepare(
    command: &Command<Value>,
    snapshot: &StationSnapshot,
    limit: &ChargingLimit,
) -> Result<Value, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    let Some(NativeProtocolReference::Ocpp201 {
        evse_id,
        connector_id,
    }) = command.resource.native_protocol_reference
    else {
        return Err(invalid);
    };
    if command.resource == snapshot.station || evse_id == 0 || evse_id > i32::MAX as u32 {
        return Err(invalid);
    }
    let mut eligible = snapshot.transactions.iter().filter(|tx| {
        matches!(tx.state, TransactionState::Pending | TransactionState::Active)
            && matches!(tx.resource.native_protocol_reference,
                Some(NativeProtocolReference::Ocpp201 { evse_id: id, connector_id: transaction_connector })
                    if id == evse_id && (connector_id.is_none() || transaction_connector == connector_id))
            && tx.resource.bridge_id == command.resource.bridge_id
            && (connector_id.is_none() || tx.resource == command.resource)
            && tx.resource.station_id == command.resource.station_id
            && tx.protocol_state.as_ref().is_some_and(|state| {
                state.protocol == ProtocolEdition::Ocpp201
                    && !state.native_transaction_id.is_empty()
                    && state.native_transaction_id.chars().count() <= 36
            })
    });
    let transaction = eligible.next().ok_or(invalid)?;
    if eligible.next().is_some() {
        return Err(invalid);
    }
    let native_id = &transaction
        .protocol_state
        .as_ref()
        .ok_or(invalid)?
        .native_transaction_id;
    let (unit, amount) = super::super::super::remote_constraints::profile_quantity(limit)?;
    // Reuse one stable profile identity for successive limits on this transaction.
    let mut hasher = Sha256::new();
    hasher.update(command.resource.station_id.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(native_id.as_bytes());
    let digest = hasher.finalize();
    let profile_id =
        (i32::from_be_bytes(digest[..4].try_into().map_err(|_| invalid)?) & i32::MAX).max(1);
    let payload = json!({
        "evseId": evse_id,
        "chargingProfile": {
            "id": profile_id,
            "stackLevel": 0,
            "chargingProfilePurpose": "TxProfile",
            "chargingProfileKind": "Relative",
            "transactionId": native_id,
            "chargingSchedule": [{
                "id": profile_id,
                "chargingRateUnit": unit,
                "chargingSchedulePeriod": [{
                    "startPeriod": 0,
                    "limit": amount,
                    "numberPhases": limit.phases,
                }]
            }]
        }
    });
    let request: SetChargingProfileRequest =
        serde_json::from_value(payload).map_err(|_| invalid)?;
    serde_json::to_value(request).map_err(|_| invalid)
}

pub(super) fn valid_response(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object
            .keys()
            .all(|key| matches!(key.as_str(), "status" | "statusInfo"))
    }) && super::mapping::valid_details(value)
        && serde_json::from_value::<SetChargingProfileResponse>(value.clone()).is_ok()
}
