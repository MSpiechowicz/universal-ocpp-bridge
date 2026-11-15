//! Bounded collection of one `NotifyMonitoringReport` series through the shared multipart path:
//! shared byte, item, fragment and time limits, shared budget reservation before sanitizing, and
//! an explicit reason for every way a report can fail to complete.
use super::device_model_collection::{json_size, progress, reason};
use crate::{
    call::reports::{FragmentMetadata, ReportRoute},
    multipart::{self, ReportLimits, ReportProgress},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;
use uob_application::{RuntimeReservation, RuntimeResourceBudget, WorkClass};
use uob_contracts::{
    DeviceReportFailure201, DeviceReportProgress201, MONITORING_REPORT_OUTPUT_LIMIT_201,
    MonitoringReportFragment201, MonitoringReportState201, ReportedMonitor201,
};

/// Slack for the result wrapper around the query, the fragments and the monitors.
const WRAPPER_BYTES: usize = 1024;
const MAXIMUM_FRAGMENTS: usize = 256;

#[cfg(test)]
#[path = "monitoring_collect_tests.rs"]
mod tests;

pub(super) struct Collected {
    pub report: MonitoringReportState201,
    /// Typed expansion and escaped persistence copies, held through the hand-off.
    pub retained: Option<(RuntimeReservation, RuntimeReservation)>,
}

pub(super) fn incomplete(
    reason: DeviceReportFailure201,
    progress: DeviceReportProgress201,
) -> Collected {
    Collected {
        report: MonitoringReportState201::Incomplete {
            reason,
            progress: Some(progress),
        },
        retained: None,
    }
}

struct Source {
    route: ReportRoute,
    fragments: Vec<MonitoringReportFragment201>,
    ingress: Option<RuntimeReservation>,
}

/// Collects until the final fragment, a shared limit or the deadline from actual dispatch.
/// `seen` records that the charger sent at least one fragment, whatever became of it.
/// `query_bytes` is the encoded request echo, which shares the result's output limit.
pub(super) async fn collect(
    mut route: ReportRoute,
    budget: RuntimeResourceBudget,
    query_bytes: usize,
    seen: Arc<AtomicBool>,
) -> Collected {
    let limits = ReportLimits::default();
    let started = match route.dispatch_started().await {
        Ok(started) => started,
        Err(failure) => return incomplete(reason(failure), progress(ReportProgress::default())),
    };
    let admission = route.take_admission();
    let Ok(metadata) = budget.try_reserve(
        WorkClass::PendingRequest,
        MAXIMUM_FRAGMENTS * (128 + std::mem::size_of::<MonitoringReportFragment201>()),
    ) else {
        return incomplete(
            DeviceReportFailure201::Capacity,
            progress(ReportProgress::default()),
        );
    };

    let source = Arc::new(Mutex::new(Source {
        route,
        fragments: Vec::with_capacity(MAXIMUM_FRAGMENTS),
        ingress: None,
    }));
    let next_source = source.clone();
    let report = multipart::collect_report(
        Ok(admission),
        limits,
        started,
        budget.clone(),
        move || {
            let source = next_source.clone();
            let seen = seen.clone();
            async move {
                let mut source = source.lock().await;
                let Some(ingress) = source.route.next().await? else {
                    return Ok(None);
                };
                seen.store(true, Ordering::Release);
                if source.fragments.len() < MAXIMUM_FRAGMENTS
                    && let FragmentMetadata::Monitoring(fragment) = ingress.metadata
                {
                    source.fragments.push(fragment);
                }
                source.ingress = Some(ingress.reservation);
                Ok(Some(ingress.fragment))
            }
        },
        std::future::pending::<()>(),
    )
    .await;
    let report = match report {
        Ok(report) => report,
        Err(partial) => {
            return incomplete(reason(partial.reason), progress(partial.progress));
        }
    };

    let collected = progress(report.progress());
    // Hold typed expansion and escaped persistence copies through the hand-off.
    let Ok(output) = budget.try_reserve(WorkClass::PendingRequest, limits.maximum_bytes * 3) else {
        return incomplete(DeviceReportFailure201::Capacity, collected);
    };
    let fragments = std::mem::take(&mut source.lock().await.fragments);
    let room = MONITORING_REPORT_OUTPUT_LIMIT_201
        .saturating_sub(query_bytes)
        .saturating_sub(json_size(&fragments, MONITORING_REPORT_OUTPUT_LIMIT_201).unwrap_or(0))
        .saturating_sub(WRAPPER_BYTES);

    let mut monitors = Vec::new();
    let mut used = 0_usize;
    let mut omitted = 0_u32;
    for item in report.items() {
        // Monitors that do not fit the result's output limit are counted, never silently dropped.
        if omitted == 0 && used + item.len() < room {
            let Ok(monitor) = serde_json::from_slice::<ReportedMonitor201>(item) else {
                return incomplete(DeviceReportFailure201::InvalidFragment, collected);
            };
            used += item.len() + 1;
            monitors.push(monitor);
        } else {
            omitted = omitted.saturating_add(1);
        }
    }
    drop(report);

    let complete = MonitoringReportState201::Complete {
        fragments,
        monitors,
        monitors_omitted: omitted,
    };
    if json_size(
        &complete,
        MONITORING_REPORT_OUTPUT_LIMIT_201.saturating_sub(query_bytes),
    )
    .is_none()
    {
        return incomplete(DeviceReportFailure201::OutputLimit, collected);
    }

    Collected {
        report: complete,
        retained: Some((metadata, output)),
    }
}
