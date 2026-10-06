//! Charger-initiated OCPP 2.0.1 charging needs, EV schedules and external limits (K11-K17).
//! Each record is committed before the CALL is answered. Nothing here admits or dispatches a
//! command, so an observed limit can never loop back to the station as a new limit.
use super::{CallContext, Clock, call_error, calls::commit_error, context};
use crate::charging::ChargingStore;
use serde_json::json;
use uob_application::{
    ChargerObservation, ChargingProfileStore201, CommandClock, CsmsSchedules201,
    NegotiationObservation201, NegotiationTransaction201, OperationalStore,
    ProfileOwnershipState201, StorageError, charging_needs_record_201, cleared_limit_record_201,
    ev_schedule_record_201, negotiation_transaction_201, record_charging_negotiation_201,
};
use uob_contracts::{
    ChargingNegotiation201, ChargingProfilePurpose201, ProtocolEdition, StationSnapshot,
};
use uob_protocol_adapter::{IncomingCall, OcppCallError, OcppErrorCode, v201};

pub(super) async fn complete(
    incoming: &IncomingCall,
    snapshot: &mut StationSnapshot,
    services: &CallContext<'_>,
) -> Result<serde_json::Value, OcppCallError> {
    let protocol = ProtocolEdition::Ocpp201;
    let internal = || call_error(protocol, OcppErrorCode::InternalError);
    let ChargerObservation::ChargingNegotiation201(observation) = &incoming.call.observation else {
        return Err(call_error(protocol, OcppErrorCode::ProtocolError));
    };
    let record = match observation.clone() {
        NegotiationObservation201::EvChargingNeeds(needs) => {
            charging_needs_record_201(snapshot, needs, services.negotiation)
        }
        NegotiationObservation201::EvChargingSchedule(schedule) => {
            let transaction = negotiation_transaction_201(snapshot, schedule.evse_id);
            let csms = match &transaction {
                Ok(transaction) => installed(services.store, snapshot, transaction)
                    .await
                    .map_err(|_| internal())?,
                Err(_) => CsmsSchedules201::Unverifiable,
            };
            ev_schedule_record_201(
                schedule,
                transaction.as_ref().map_err(|reason| *reason),
                &csms,
            )
        }
        NegotiationObservation201::ChargingLimit(limit) => {
            ChargingNegotiation201::ChargingLimit { limit }
        }
        NegotiationObservation201::ChargingLimitCleared(cleared) => {
            cleared_limit_record_201(snapshot, cleared)
        }
    };
    let response = v201::charging_negotiation::response(&record);
    let context = context(
        services.store,
        services.identity,
        incoming,
        services.target.clone(),
    )
    .await
    .map_err(|_| internal())?;
    record_charging_negotiation_201(services.store, snapshot, record, context, Clock.now())
        .await
        .map_err(|error| commit_error(protocol, &error))?;
    Ok(json!([3, incoming.call.message_id, response]))
}

/// The `TxProfile`s this bridge installed for the transaction, rebuilt from their retained
/// requests. Any in-flight or uncertain mutation, or an unretained or unparseable request,
/// makes the exact check impossible.
async fn installed(
    store: &ChargingStore,
    snapshot: &StationSnapshot,
    transaction: &NegotiationTransaction201,
) -> Result<CsmsSchedules201, StorageError> {
    let ledger = store
        .charging_profile_owners(snapshot.station.clone())
        .await?;
    let owners: Vec<_> = ledger
        .owners
        .into_iter()
        .filter(|owner| {
            owner.footprint.purpose == ChargingProfilePurpose201::TxProfile
                && owner.footprint.transaction_id.as_deref()
                    == Some(transaction.native_transaction_id.as_str())
        })
        .collect();
    if ledger.busy
        || owners
            .iter()
            .any(|owner| owner.state != ProfileOwnershipState201::Owned)
    {
        return Ok(CsmsSchedules201::Unverifiable);
    }
    let mut profiles = Vec::with_capacity(owners.len());
    for owner in owners {
        let Some(command) = store.command_by_request_id(owner.request_id).await? else {
            return Ok(CsmsSchedules201::Unverifiable);
        };
        let Some(profile) =
            v201::charging_negotiation::csms_tx_profile(&command, owner.footprint.id).filter(
                |profile| {
                    profile.stack_level == owner.footprint.stack_level
                        && profile.valid_from == owner.footprint.valid_from
                        && profile.valid_to == owner.footprint.valid_to
                },
            )
        else {
            return Ok(CsmsSchedules201::Unverifiable);
        };
        profiles.push(profile);
    }
    Ok(CsmsSchedules201::Known(profiles))
}
