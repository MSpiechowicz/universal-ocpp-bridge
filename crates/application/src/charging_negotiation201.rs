//! Application-owned OCPP 2.0.1 charging-needs, EV-schedule and external-limit coordination
//! (K11-K17). The bridge records charger facts and answers within its own authority; it never
//! calculates a schedule, never enforces a limit and never turns an observation into a command.
mod evaluation;
mod persistence;
pub use persistence::record_charging_negotiation_201;

use uob_contracts::{
    ChargingLimitSource201, ChargingNegotiation201, ChargingProfileKind201, ChargingSchedule201,
    ClearedChargingLimit201, EvChargingNeeds201, EvChargingNeedsStatus201, EvChargingSchedule201,
    EvChargingScheduleStatus201, EvScheduleBasis201, ExternalChargingLimit201,
    NativeProtocolReference, NegotiationReason201, ProtocolEdition, StationSnapshot, TransactionId,
    TransactionState, TypedValue, UtcTimestamp,
};

/// Validated charger-initiated notification awaiting the bridge's decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NegotiationObservation201 {
    EvChargingNeeds(EvChargingNeeds201),
    /// Decoded with its complete schedule; only durable evidence may omit it.
    EvChargingSchedule(EvChargingSchedule201),
    ChargingLimit(ExternalChargingLimit201),
    ChargingLimitCleared(ClearedChargingLimit201),
}

/// Per-station operator policy. The default answers every charging need with `Rejected`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NegotiationPolicy201 {
    /// Operator assertion that its EMS sends a `TxProfile` through this bridge after charging
    /// needs (K15.FR.05/07/08); the bridge itself never computes one.
    pub charging_needs_processing: bool,
}

/// The single current OCPP 2.0.1 transaction on a configured EVSE.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiationTransaction201 {
    pub transaction_id: TransactionId,
    pub native_transaction_id: String,
}

/// One `TxProfile` the bridge installed for the transaction, with its single schedule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsmsTxProfile201 {
    pub stack_level: i32,
    pub kind: ChargingProfileKind201,
    pub valid_from: Option<UtcTimestamp>,
    pub valid_to: Option<UtcTimestamp>,
    pub schedule: ChargingSchedule201,
}

/// The bridge's own `TxProfile` limits for one transaction, as far as they are exactly known.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CsmsSchedules201 {
    /// Every owned `TxProfile` is confirmed and its request is retained; empty means none.
    Known(Vec<CsmsTxProfile201>),
    /// An in-flight, uncertain or unretained profile prevents an exact check.
    Unverifiable,
}

/// Resolves the configured EVSE and its single current OCPP 2.0.1 transaction.
///
/// # Errors
///
/// Returns `UnknownEvse` without an exact configured EVSE resource and `TxNotFound` unless
/// exactly one pending, active or suspended OCPP 2.0.1 transaction is retained on it; an
/// uncertain transaction is not current.
pub fn negotiation_transaction_201(
    snapshot: &StationSnapshot,
    evse_id: u32,
) -> Result<NegotiationTransaction201, NegotiationReason201> {
    if !evse_configured(snapshot, evse_id) {
        return Err(NegotiationReason201::UnknownEvse);
    }
    let mut current = snapshot.transactions.iter().filter_map(|transaction| {
        let state = transaction.protocol_state.as_ref()?;
        let on_evse = matches!(
            transaction.resource.native_protocol_reference,
            Some(NativeProtocolReference::Ocpp201 { evse_id: id, .. }) if id == evse_id
        );
        (matches!(
            transaction.state,
            TransactionState::Pending | TransactionState::Active | TransactionState::Suspended
        ) && state.protocol == ProtocolEdition::Ocpp201
            && on_evse
            && !state.native_transaction_id.is_empty())
        .then(|| NegotiationTransaction201 {
            transaction_id: transaction.transaction_id.clone(),
            native_transaction_id: state.native_transaction_id.clone(),
        })
    });
    match (current.next(), current.next()) {
        (Some(transaction), None) => Ok(transaction),
        _ => Err(NegotiationReason201::TxNotFound),
    }
}

/// Answers charging needs (K15.FR.02-05, K17.FR.02-05). The bridge never claims `Accepted`:
/// it has no schedule of its own, and only an operator-asserted EMS can supply one later.
#[must_use]
pub fn charging_needs_record_201(
    snapshot: &StationSnapshot,
    needs: EvChargingNeeds201,
    policy: NegotiationPolicy201,
) -> ChargingNegotiation201 {
    let (status, reason, transaction_id) =
        match negotiation_transaction_201(snapshot, needs.evse_id) {
            Err(reason) => (EvChargingNeedsStatus201::Rejected, Some(reason), None),
            Ok(transaction) if policy.charging_needs_processing => (
                EvChargingNeedsStatus201::Processing,
                None,
                Some(transaction.transaction_id),
            ),
            Ok(transaction) => (
                EvChargingNeedsStatus201::Rejected,
                Some(NegotiationReason201::NotEnabled),
                Some(transaction.transaction_id),
            ),
        };
    ChargingNegotiation201::EvChargingNeeds {
        needs,
        status,
        reason,
        transaction_id,
    }
}

/// Checks an EV schedule against the bridge's own installed `TxProfiles` (K15.FR.11/12,
/// K16.FR.06/07, K17.FR.11/12). Anything it cannot check exactly is `Rejected`.
#[must_use]
pub fn ev_schedule_record_201(
    schedule: EvChargingSchedule201,
    transaction: Result<&NegotiationTransaction201, NegotiationReason201>,
    csms: &CsmsSchedules201,
) -> ChargingNegotiation201 {
    let (basis, transaction_id) = match transaction {
        Err(NegotiationReason201::UnknownEvse) => (EvScheduleBasis201::UnknownEvse, None),
        Err(_) => (EvScheduleBasis201::NoTransaction, None),
        Ok(transaction) => (
            evaluation::evaluate(&schedule, csms),
            Some(transaction.transaction_id.clone()),
        ),
    };
    let (status, reason) = match basis {
        EvScheduleBasis201::NoCsmsSchedule | EvScheduleBasis201::WithinCsmsSchedule => {
            (EvChargingScheduleStatus201::Accepted, None)
        }
        EvScheduleBasis201::ExceedsCsmsSchedule => (
            EvChargingScheduleStatus201::Rejected,
            Some(NegotiationReason201::ValueTooHigh),
        ),
        EvScheduleBasis201::Unverifiable => (
            EvChargingScheduleStatus201::Rejected,
            Some(NegotiationReason201::Unspecified),
        ),
        EvScheduleBasis201::UnknownEvse => (
            EvChargingScheduleStatus201::Rejected,
            Some(NegotiationReason201::UnknownEvse),
        ),
        EvScheduleBasis201::NoTransaction => (
            EvChargingScheduleStatus201::Rejected,
            Some(NegotiationReason201::TxNotFound),
        ),
    };
    ChargingNegotiation201::EvChargingSchedule {
        schedule,
        status,
        basis,
        reason,
        transaction_id,
    }
}

/// Records a release (K13.FR.02, K14.FR.03) and whether a matching limit was active.
#[must_use]
pub fn cleared_limit_record_201(
    snapshot: &StationSnapshot,
    cleared: ClearedChargingLimit201,
) -> ChargingNegotiation201 {
    let scope = cleared.evse_id.filter(|id| *id > 0);
    let active = limit_point(scope, cleared.charging_limit_source, "active");
    let values = match scope {
        None => Some(&snapshot.current_values),
        Some(evse_id) => evse_resource(snapshot, evse_id).map(|resource| &resource.current_values),
    };
    let released = values.is_some_and(|values| {
        values.iter().any(|value| {
            value.point_id.as_str() == active && value.value == Some(TypedValue::Boolean(true))
        })
    });
    ChargingNegotiation201::ChargingLimitCleared { cleared, released }
}

fn evse_configured(snapshot: &StationSnapshot, evse_id: u32) -> bool {
    evse_id > 0 && evse_resource(snapshot, evse_id).is_some()
}

fn evse_resource(
    snapshot: &StationSnapshot,
    evse_id: u32,
) -> Option<&uob_contracts::ChargingResourceSnapshot> {
    snapshot.resources.iter().find(|resource| {
        resource.resource.native_protocol_reference
            == Some(NativeProtocolReference::Ocpp201 {
                evse_id,
                connector_id: None,
            })
    })
}

/// Point names are EVSE-qualified even on the EVSE resource, as connector status points are.
fn limit_point(evse_id: Option<u32>, source: ChargingLimitSource201, field: &str) -> String {
    let source = match source {
        ChargingLimitSource201::Ems => "EMS",
        ChargingLimitSource201::Other => "Other",
        ChargingLimitSource201::So => "SO",
        ChargingLimitSource201::Cso => "CSO",
    };
    evse_id.map_or_else(
        || format!("ocpp201/charging-limit/{source}/{field}"),
        |evse_id| format!("ocpp201/evse-{evse_id}/charging-limit/{source}/{field}"),
    )
}
