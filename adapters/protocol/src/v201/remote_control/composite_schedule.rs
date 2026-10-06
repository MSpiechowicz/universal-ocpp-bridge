//! Indicative OCPP 2.0.1 `GetCompositeSchedule` replies (K08), never local calculation.
use super::{mapping, schedule_values as values};
use crate::command_registry::composite_schedule201 as registry;
use serde::Deserialize;
use serde_json::Value;
use uob_application::CommandDispatchOutcome;
use uob_contracts::{
    ChargingScheduleRateUnit201, Command, CommandErrorCode, CommandOperation, CompositeSchedule201,
    CompositeScheduleRequest201, CompositeScheduleResult201, CompositeScheduleStatus201,
    SmartChargingReason201, UtcTimestamp,
};

pub(super) fn request_context(
    command: &Command<Value>,
) -> Result<Option<CompositeScheduleRequest201>, CommandErrorCode> {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return Ok(None);
    };
    if operation.action.as_str() != registry::ACTION {
        return Ok(None);
    }
    registry::validate(&command.resource, operation).map(Some)
}

pub(super) fn response(
    request: CompositeScheduleRequest201,
    payload: &Value,
) -> CommandDispatchOutcome {
    parse(request, payload).map_or_else(
        mapping::uncertain,
        CommandDispatchOutcome::CompositeScheduleResponse201,
    )
}

pub(super) fn reason(payload: &Value) -> Option<SmartChargingReason201> {
    payload["statusInfo"]["reasonCode"]
        .as_str()
        .and_then(SmartChargingReason201::from_native)
}

fn parse(
    request: CompositeScheduleRequest201,
    payload: &Value,
) -> Option<CompositeScheduleResult201> {
    if !registry::valid_schema(registry::RESPONSE, payload) {
        return None;
    }
    let status = CompositeScheduleStatus201::deserialize(&payload["status"]).ok()?;
    let schedule = payload
        .get("schedule")
        .map(|raw| schedule(&request, raw))
        .map_or(Some(None), |schedule| schedule.map(Some))?;
    // The schedule may only be omitted with Rejected; any supplied one must still be valid.
    if status == CompositeScheduleStatus201::Accepted && schedule.is_none() {
        return None;
    }
    Some(CompositeScheduleResult201 {
        request,
        status,
        reason_code: reason(payload),
        schedule,
    })
}

fn schedule(request: &CompositeScheduleRequest201, raw: &Value) -> Option<CompositeSchedule201> {
    let evse_id = values::integer(&raw["evseId"])?;
    let duration = values::integer(&raw["duration"])?;
    let schedule_start = UtcTimestamp::deserialize(&raw["scheduleStart"]).ok()?;
    let charging_rate_unit =
        ChargingScheduleRateUnit201::deserialize(&raw["chargingRateUnit"]).ok()?;
    // K08.FR.02 bounds the calculation by the requested duration; K08.FR.07 rejects rather
    // than substituting another unit.
    if evse_id != request.evse_id
        || duration <= 0
        || duration > request.duration
        || request
            .charging_rate_unit
            .is_some_and(|unit| unit != charging_rate_unit)
    {
        return None;
    }
    values::horizon(schedule_start, duration)?;
    Some(CompositeSchedule201 {
        evse_id,
        duration,
        schedule_start,
        charging_rate_unit,
        charging_schedule_period: values::periods(&raw["chargingSchedulePeriod"], Some(duration))?,
    })
}
