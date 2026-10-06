//! Exact comparison of an EV schedule with the bridge's own installed `TxProfile` stack.
//!
//! Every placed limit is a half-open interval in nanoseconds. Between consecutive breakpoints
//! both sides are constant, so checking each breakpoint decides the whole horizon. Only
//! profiles that can be placed in time exactly take part; anything else is unverifiable.
use super::{CsmsSchedules201, CsmsTxProfile201};
use uob_contracts::{
    ChargingProfileKind201, ChargingSchedule201, EvChargingSchedule201, EvScheduleBasis201,
    ExactDecimal, UtcTimestamp,
};

const NANOS_PER_SECOND: i128 = 1_000_000_000;

#[derive(Clone, Copy)]
struct Segment {
    start: i128,
    end: i128,
    tenths: i128,
}

pub(super) fn evaluate(ev: &EvChargingSchedule201, csms: &CsmsSchedules201) -> EvScheduleBasis201 {
    let CsmsSchedules201::Known(profiles) = csms else {
        return EvScheduleBasis201::Unverifiable;
    };
    if profiles.is_empty() {
        return EvScheduleBasis201::NoCsmsSchedule;
    }
    let Some(schedule) = &ev.charging_schedule else {
        return EvScheduleBasis201::Unverifiable;
    };
    // Amperes and watts are never converted using guessed voltages or phase counts.
    if profiles
        .iter()
        .any(|profile| profile.schedule.charging_rate_unit != schedule.charging_rate_unit)
    {
        return EvScheduleBasis201::Unverifiable;
    }
    let Some(requested) = segments(Some(nanos(ev.time_base)), schedule) else {
        return EvScheduleBasis201::Unverifiable;
    };
    let Some(installed) = profiles.iter().map(placed).collect::<Option<Vec<_>>>() else {
        return EvScheduleBasis201::Unverifiable;
    };
    let mut breakpoints: Vec<i128> = requested.iter().map(|segment| segment.start).collect();
    for (_, segments) in &installed {
        for segment in segments {
            breakpoints.extend([segment.start, segment.end]);
        }
    }
    breakpoints.retain(|point| *point != i128::MIN && *point != i128::MAX);
    breakpoints.sort_unstable();
    breakpoints.dedup();
    for point in breakpoints {
        let Some(requested) = limit_at(&requested, point) else {
            continue;
        };
        let top = installed
            .iter()
            .filter(|(_, segments)| limit_at(segments, point).is_some())
            .map(|(stack, _)| *stack)
            .max();
        // Equal stack levels for one transaction are refused at admission; stay conservative.
        let allowed = installed
            .iter()
            .filter(|(stack, _)| Some(*stack) == top)
            .filter_map(|(_, segments)| limit_at(segments, point))
            .min();
        if allowed.is_some_and(|allowed| requested > allowed) {
            return EvScheduleBasis201::ExceedsCsmsSchedule;
        }
    }
    EvScheduleBasis201::WithinCsmsSchedule
}

/// Absolute schedules are anchored at `startSchedule`. A relative schedule's start is the
/// station's `PowerPathClosed` moment, which the bridge does not know, so only a single
/// unbounded period (such as a canonical charging limit) is time-invariant and placeable.
fn placed(profile: &CsmsTxProfile201) -> Option<(i32, Vec<Segment>)> {
    let segments = match profile.kind {
        ChargingProfileKind201::Absolute => segments(
            Some(nanos(profile.schedule.start_schedule?)),
            &profile.schedule,
        )?,
        ChargingProfileKind201::Relative => segments(None, &profile.schedule)?,
        ChargingProfileKind201::Recurring => return None,
    };
    let from = profile.valid_from.map_or(i128::MIN, nanos);
    let to = profile.valid_to.map_or(i128::MAX, nanos);
    let clipped = segments
        .into_iter()
        .filter_map(|segment| {
            let start = segment.start.max(from);
            let end = segment.end.min(to);
            (start < end).then_some(Segment {
                start,
                end,
                tenths: segment.tenths,
            })
        })
        .collect();
    Some((profile.stack_level, clipped))
}

/// Ordered half-open periods; a supplied duration truncates the last ones.
fn segments(anchor: Option<i128>, schedule: &ChargingSchedule201) -> Option<Vec<Segment>> {
    let periods = &schedule.charging_schedule_period;
    let Some(anchor) = anchor else {
        return match (periods.as_slice(), schedule.duration) {
            ([period], None) => Some(vec![Segment {
                start: i128::MIN,
                end: i128::MAX,
                tenths: tenths(period.limit)?,
            }]),
            _ => None,
        };
    };
    let horizon = schedule
        .duration
        .map(|seconds| anchor + i128::from(seconds) * NANOS_PER_SECOND);
    let mut result = Vec::with_capacity(periods.len());
    for (index, period) in periods.iter().enumerate() {
        let start = anchor + i128::from(period.start_period) * NANOS_PER_SECOND;
        let next = periods.get(index + 1).map_or(i128::MAX, |next| {
            anchor + i128::from(next.start_period) * NANOS_PER_SECOND
        });
        let end = horizon.map_or(next, |horizon| next.min(horizon));
        if start < end {
            result.push(Segment {
                start,
                end,
                tenths: tenths(period.limit)?,
            });
        }
    }
    Some(result)
}

fn limit_at(segments: &[Segment], point: i128) -> Option<i128> {
    let index = segments.partition_point(|segment| segment.start <= point);
    let segment = segments.get(index.checked_sub(1)?)?;
    (point < segment.end).then_some(segment.tenths)
}

/// Schedule limits are exact tenths (`ChargingSchedulePeriodType.limit`).
fn tenths(value: ExactDecimal) -> Option<i128> {
    match value.scale() {
        0 => value.coefficient().checked_mul(10),
        1 => Some(value.coefficient()),
        _ => None,
    }
}

fn nanos(timestamp: UtcTimestamp) -> i128 {
    timestamp.into_inner().unix_timestamp_nanos()
}
