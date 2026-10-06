use crate::{
    call::reports::ReportRoute,
    multipart::{self, ReportFailure, ReportLimits, ReportProgress},
};
use std::{io, sync::Arc};
use tokio::sync::{Mutex, oneshot};
use uob_application::{RuntimeReservation, RuntimeResourceBudget, WorkClass};
use uob_contracts::{
    DeviceReportFailure201, DeviceReportFragment201, DeviceReportItem201, DeviceReportProgress201,
    DeviceReportState201,
};

struct Source {
    route: ReportRoute,
    metadata: Vec<DeviceReportFragment201>,
    ingress: Option<RuntimeReservation>,
}
fn progress(progress: ReportProgress) -> DeviceReportProgress201 {
    DeviceReportProgress201 {
        fragments: progress.fragments,
        items: progress.items,
        bytes: progress.bytes,
    }
}
fn reason(reason: ReportFailure) -> DeviceReportFailure201 {
    match reason {
        ReportFailure::TimedOut => DeviceReportFailure201::Timeout,
        ReportFailure::ByteLimit => DeviceReportFailure201::ByteLimit,
        ReportFailure::ItemLimit => DeviceReportFailure201::ItemLimit,
        ReportFailure::FragmentLimit => DeviceReportFailure201::FragmentLimit,
        ReportFailure::Capacity(error)
            if error.limit == uob_application::AdmissionLimit::OcppMessageBytes =>
        {
            DeviceReportFailure201::ByteLimit
        }
        ReportFailure::Capacity(_) => DeviceReportFailure201::Capacity,
        ReportFailure::CorrelationMismatch => DeviceReportFailure201::Correlation,
        ReportFailure::DuplicateOrConflictingSequence => DeviceReportFailure201::DuplicateSequence,
        ReportFailure::MissingOrOutOfOrderSequence => DeviceReportFailure201::OutOfOrderSequence,
        ReportFailure::Disconnected => DeviceReportFailure201::Disconnected,
        ReportFailure::NotTransmitted => DeviceReportFailure201::NotTransmitted,
        ReportFailure::Cancelled => DeviceReportFailure201::NativeRejected,
        ReportFailure::InvalidConfiguration | ReportFailure::InvalidFragment => {
            DeviceReportFailure201::InvalidFragment
        }
    }
}
struct Counter {
    bytes: usize,
    maximum: usize,
}
impl io::Write for Counter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .filter(|bytes| *bytes <= self.maximum)
            .ok_or_else(|| io::Error::other("sanitized report output limit"))?;
        Ok(buffer.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Count complete escaped JSON without allocating another copy.
pub(super) fn json_size(value: &impl serde::Serialize, maximum: usize) -> Option<usize> {
    let mut counter = Counter { bytes: 0, maximum };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.bytes)
}
pub(super) struct CollectedDevice201 {
    pub report: DeviceReportState201,
    pub retained: Option<(RuntimeReservation, RuntimeReservation)>,
}
fn incomplete(
    reason: DeviceReportFailure201,
    progress: DeviceReportProgress201,
) -> CollectedDevice201 {
    CollectedDevice201 {
        report: DeviceReportState201::Incomplete {
            reason,
            progress: Some(progress),
        },
        retained: None,
    }
}
pub(super) async fn collect(
    mut route: ReportRoute,
    budget: RuntimeResourceBudget,
    cancelled: oneshot::Receiver<()>,
) -> CollectedDevice201 {
    let limits = ReportLimits::default();
    let started = match route.dispatch_started().await {
        Ok(started) => started,
        Err(failure) => return incomplete(reason(failure), progress(ReportProgress::default())),
    };
    let admission = route.take_admission();
    let Ok(metadata) = budget.try_reserve(
        WorkClass::PendingRequest,
        256 * (128 + std::mem::size_of::<DeviceReportFragment201>()),
    ) else {
        return incomplete(
            DeviceReportFailure201::Capacity,
            progress(ReportProgress::default()),
        );
    };
    let source = Arc::new(Mutex::new(Source {
        route,
        metadata: Vec::with_capacity(256),
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
            async move {
                let mut source = source.lock().await;
                let Some(ingress) = source.route.next().await? else {
                    return Ok(None);
                };
                if source.metadata.len() < 256
                    && let crate::call::reports::FragmentMetadata::Device(metadata) =
                        ingress.metadata
                {
                    source.metadata.push(metadata);
                }
                source.ingress = Some(ingress.reservation);
                Ok(Some(ingress.fragment))
            }
        },
        async move {
            if cancelled.await.is_err() {
                std::future::pending::<()>().await;
            }
        },
    )
    .await;
    let report = match report {
        Ok(report) => report,
        Err(partial) => return incomplete(reason(partial.reason), progress(partial.progress)),
    };
    let progress = progress(report.progress());
    // Hold typed expansion and escaped persistence copies through the durable write.
    let Ok(output) = budget.try_reserve(WorkClass::PendingRequest, limits.maximum_bytes * 3) else {
        return incomplete(DeviceReportFailure201::Capacity, progress);
    };
    let items = report
        .items()
        .map(serde_json::from_slice::<DeviceReportItem201>)
        .collect::<Result<Vec<_>, _>>();
    let Ok(items) = items else {
        return incomplete(DeviceReportFailure201::InvalidFragment, progress);
    };
    drop(report);
    let fragments = std::mem::take(&mut source.lock().await.metadata);
    let complete = DeviceReportState201::Complete {
        progress,
        fragments,
        items,
    };
    if json_size(&complete, limits.maximum_bytes).is_none() {
        return incomplete(DeviceReportFailure201::OutputLimit, progress);
    }
    CollectedDevice201 {
        report: complete,
        retained: Some((metadata, output)),
    }
}
