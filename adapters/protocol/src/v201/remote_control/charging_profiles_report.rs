//! `ReportChargingProfilesRequest` fragments (K09) sanitized against their exact query, and
//! the bounded collection of one report. Reported profiles are never adopted as local policy.
use super::schedule_values as values;
use crate::{
    call::reports::{FragmentMetadata, ReportRoute},
    multipart::{self, ReportFailure, ReportLimits, ReportProgress},
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use uob_application::{RuntimeReservation, RuntimeResourceBudget, WorkClass};
use uob_contracts::{
    CHARGING_PROFILE_REPORT_OUTPUT_LIMIT_201, ChargingLimitSource201, ChargingProfileKind201,
    ChargingProfileRecurrency201, ChargingProfileReportFailure201,
    ChargingProfileReportFragment201, ChargingProfileReportProgress201,
    ChargingProfileReportState201, ChargingProfilesQuery201, ReportedChargingProfile201,
    ReportedChargingProfilePurpose201,
};

pub(crate) struct SanitizedFragment {
    pub items: Vec<Vec<u8>>,
    pub metadata: ChargingProfileReportFragment201,
}

/// The caller validated the pinned schema and request ID. Out-of-query EVSEs, sources or
/// profiles are correlation failures (K09.FR.04-06); malformed profiles are invalid fragments.
pub(crate) fn fragment(
    payload: &Value,
    query: &ChargingProfilesQuery201,
    sequence: u32,
) -> Result<SanitizedFragment, ReportFailure> {
    let evse_id = values::integer(&payload["evseId"]).ok_or(ReportFailure::InvalidFragment)?;
    let source = ChargingLimitSource201::deserialize(&payload["chargingLimitSource"])
        .map_err(|_| ReportFailure::InvalidFragment)?;
    if evse_id < 0
        || query.evse_id.is_some_and(|id| id != evse_id)
        || (!query.charging_limit_source.is_empty()
            && !query.charging_limit_source.contains(&source))
    {
        return Err(ReportFailure::CorrelationMismatch);
    }
    let raw = payload["chargingProfile"]
        .as_array()
        .ok_or(ReportFailure::InvalidFragment)?;
    let mut items = Vec::with_capacity(raw.len());
    for raw in raw {
        let profile = profile(raw, evse_id, source).ok_or(ReportFailure::InvalidFragment)?;
        if !matches(query, &profile) {
            return Err(ReportFailure::CorrelationMismatch);
        }
        items.push(serde_json::to_vec(&profile).map_err(|_| ReportFailure::ByteLimit)?);
    }
    Ok(SanitizedFragment {
        metadata: ChargingProfileReportFragment201 {
            sequence,
            evse_id,
            charging_limit_source: source,
            more: payload.get("tbc").and_then(Value::as_bool).unwrap_or(false),
            profiles: items.len(),
        },
        items,
    })
}

/// Profile IDs exclude the other criteria (K09.FR.03); otherwise every supplied field matches.
fn matches(query: &ChargingProfilesQuery201, profile: &ReportedChargingProfile201) -> bool {
    if !query.charging_profile_id.is_empty() {
        return query.charging_profile_id.contains(&profile.id);
    }
    query
        .charging_profile_purpose
        .is_none_or(|purpose| purpose == profile.charging_profile_purpose)
        && query
            .stack_level
            .is_none_or(|level| level == profile.stack_level)
}

fn profile(
    raw: &Value,
    evse_id: i32,
    charging_limit_source: ChargingLimitSource201,
) -> Option<ReportedChargingProfile201> {
    let stack_level = values::integer(&raw["stackLevel"]).filter(|level| *level >= 0)?;
    let charging_profile_kind =
        ChargingProfileKind201::deserialize(&raw["chargingProfileKind"]).ok()?;
    let recurrency_kind = raw
        .get("recurrencyKind")
        .map(ChargingProfileRecurrency201::deserialize)
        .transpose()
        .ok()?;
    let transaction_id = match raw.get("transactionId") {
        None => None,
        Some(id) => Some(id.as_str().filter(|id| !id.is_empty())?.to_owned()),
    };
    let valid_from = values::timestamp(raw.get("validFrom")).ok()?;
    let valid_to = values::timestamp(raw.get("validTo")).ok()?;
    if (charging_profile_kind == ChargingProfileKind201::Recurring) != recurrency_kind.is_some()
        || matches!((valid_from, valid_to), (Some(from), Some(to)) if from >= to)
    {
        return None;
    }
    let mut sales_tariff_omitted = false;
    let mut charging_schedule = Vec::new();
    for schedule in raw["chargingSchedule"].as_array()? {
        let (schedule, tariff) = values::schedule(schedule)?;
        sales_tariff_omitted |= tariff;
        charging_schedule.push(schedule);
    }
    Some(ReportedChargingProfile201 {
        evse_id,
        charging_limit_source,
        id: values::integer(&raw["id"])?,
        stack_level,
        charging_profile_purpose: ReportedChargingProfilePurpose201::deserialize(
            &raw["chargingProfilePurpose"],
        )
        .ok()?,
        charging_profile_kind,
        transaction_id,
        recurrency_kind,
        valid_from,
        valid_to,
        charging_schedule,
        sales_tariff_omitted,
    })
}

const fn progress(progress: ReportProgress) -> ChargingProfileReportProgress201 {
    ChargingProfileReportProgress201 {
        fragments: progress.fragments,
        profiles: progress.items,
        bytes: progress.bytes,
    }
}

fn reason(reason: ReportFailure) -> ChargingProfileReportFailure201 {
    use ChargingProfileReportFailure201 as Failure;
    match reason {
        ReportFailure::TimedOut => Failure::Timeout,
        ReportFailure::ByteLimit => Failure::ByteLimit,
        ReportFailure::ItemLimit => Failure::ItemLimit,
        ReportFailure::FragmentLimit => Failure::FragmentLimit,
        ReportFailure::Capacity(error)
            if error.limit == uob_application::AdmissionLimit::OcppMessageBytes =>
        {
            Failure::ByteLimit
        }
        ReportFailure::Capacity(_) => Failure::Capacity,
        ReportFailure::CorrelationMismatch => Failure::Correlation,
        ReportFailure::Disconnected | ReportFailure::NotTransmitted | ReportFailure::Cancelled => {
            Failure::Disconnected
        }
        // Arrival order assigns sequences, so sequence failures mean an inconsistent fragment.
        ReportFailure::DuplicateOrConflictingSequence
        | ReportFailure::MissingOrOutOfOrderSequence
        | ReportFailure::InvalidConfiguration
        | ReportFailure::InvalidFragment => Failure::InvalidFragment,
    }
}

pub(super) struct CollectedProfiles201 {
    pub report: ChargingProfileReportState201,
    /// Typed expansion and escaped persistence copies, held through the durable write.
    pub retained: Option<(RuntimeReservation, RuntimeReservation)>,
}

fn incomplete(
    reason: ChargingProfileReportFailure201,
    progress: ChargingProfileReportProgress201,
) -> CollectedProfiles201 {
    CollectedProfiles201 {
        report: ChargingProfileReportState201::Incomplete {
            reason,
            progress: Some(progress),
        },
        retained: None,
    }
}

/// Collect until the final fragment, a shared limit or the deadline from actual dispatch.
pub(super) async fn collect(
    mut route: ReportRoute,
    budget: RuntimeResourceBudget,
) -> CollectedProfiles201 {
    let limits = ReportLimits::default();
    let started = match route.dispatch_started().await {
        Ok(started) => started,
        Err(failure) => return incomplete(reason(failure), progress(ReportProgress::default())),
    };
    let admission = route.take_admission();
    let fragment_bytes = 256 * (64 + std::mem::size_of::<ChargingProfileReportFragment201>());
    let Ok(metadata) = budget.try_reserve(WorkClass::PendingRequest, fragment_bytes) else {
        return incomplete(
            ChargingProfileReportFailure201::Capacity,
            progress(ReportProgress::default()),
        );
    };
    let source = Arc::new(Mutex::new((route, Vec::with_capacity(256), None)));
    let next_source = source.clone();
    let report = multipart::collect_report(
        Ok(admission),
        limits,
        started,
        budget.clone(),
        move || {
            let source = next_source.clone();
            async move {
                let mut source = source.lock().await;
                let Some(ingress) = source.0.next().await? else {
                    return Ok(None);
                };
                if let FragmentMetadata::ChargingProfiles(fragment) = ingress.metadata
                    && source.1.len() < 256
                {
                    source.1.push(fragment);
                }
                source.2 = Some(ingress.reservation);
                Ok(Some(ingress.fragment))
            }
        },
        std::future::pending::<()>(),
    )
    .await;
    let report = match report {
        Ok(report) => report,
        Err(partial) => return incomplete(reason(partial.reason), progress(partial.progress)),
    };
    let progress = progress(report.progress());
    let Ok(output) = budget.try_reserve(WorkClass::PendingRequest, limits.maximum_bytes * 3) else {
        return incomplete(ChargingProfileReportFailure201::Capacity, progress);
    };
    let profiles = report
        .items()
        .map(serde_json::from_slice::<ReportedChargingProfile201>)
        .collect::<Result<Vec<_>, _>>();
    let Ok(profiles) = profiles else {
        return incomplete(ChargingProfileReportFailure201::InvalidFragment, progress);
    };
    drop(report);
    let fragments = std::mem::take(&mut source.lock().await.1);
    let complete = ChargingProfileReportState201::Complete {
        progress,
        fragments,
        profiles,
    };
    if super::device_model_collection::json_size(
        &complete,
        CHARGING_PROFILE_REPORT_OUTPUT_LIMIT_201,
    )
    .is_none()
    {
        return incomplete(ChargingProfileReportFailure201::OutputLimit, progress);
    }
    CollectedProfiles201 {
        report: complete,
        retained: Some((metadata, output)),
    }
}
