//! Task-free, bounded multipart collection for native report workflow owners.
//! Device-model and monitoring business workflows remain separate consumers.
mod types;
pub use types::*;

use std::{future::Future, mem::size_of};
use tokio::time::{Instant, sleep_until};
use uob_application::{RuntimeResourceBudget, WorkClass};

/// Collect one correlated report, beginning at sequence zero, until its final fragment.
///
/// `next` supplies already validated, frame-bounded native fragments from the existing
/// socket owner, not an unbounded channel. The source must be cancellation-safe and
/// cooperative (never block a runtime thread), and unregister its request route when
/// dropped. This function owns/drops the source on every exit and spawns no task.
/// `cancel` is a workflow-owned cancellation future; disconnect is `Ok(None)`.
/// The requesting workflow registers one collector per connection/kind/request ID
/// before dispatch and must never reuse that ID within the connection.
///
/// # Errors
/// Returns explicit partial/truncated counts for invalid input, sequence/correlation
/// errors, limits, timeout, cancellation or disconnect. All partial buffers and shared
/// reservations are released before returning; only complete content retains a guard.
pub async fn collect_report<N, F, C>(
    key: ReportKey,
    limits: ReportLimits,
    budget: RuntimeResourceBudget,
    mut next: N,
    cancel: C,
) -> Result<CollectedReport, Box<PartialReport>>
where
    N: FnMut() -> F,
    F: Future<Output = Result<Option<ReportFragment>, ReportFailure>>,
    C: Future<Output = ()>,
{
    let mut report = begin(key, limits, &budget)?;
    let deadline = Instant::now() + limits.timeout;
    let timer = sleep_until(deadline);
    tokio::pin!(timer, cancel);
    loop {
        let fragment = tokio::select! {
            biased;
            () = &mut cancel => Err(ReportFailure::Cancelled),
            () = &mut timer => Err(ReportFailure::TimedOut),
            fragment = next() => fragment.and_then(|f| f.ok_or(ReportFailure::Disconnected)),
        };
        let accepted = fragment.and_then(|fragment| {
            // A continuously-ready source cannot win after the absolute deadline.
            if Instant::now() >= deadline {
                return Err(ReportFailure::TimedOut);
            }
            append(&mut report, &fragment, limits, &budget)
        });
        match accepted {
            Ok(true) => return Ok(report),
            Ok(false) => tokio::task::yield_now().await,
            Err(reason) => {
                return Err(Box::new(PartialReport {
                    key: report.key.clone(),
                    progress: report.progress,
                    reason,
                }));
            }
        }
    }
}

fn begin(
    key: ReportKey,
    limits: ReportLimits,
    budget: &RuntimeResourceBudget,
) -> Result<CollectedReport, Box<PartialReport>> {
    let reserve = || {
        if !limits.valid() || !key.valid() {
            return Err(ReportFailure::InvalidConfiguration);
        }
        // Reserve the fixed descriptor array and bounded correlation metadata before
        // allocating. Payload accounting then grows before copying each fragment.
        budget
            .try_reserve(WorkClass::MultipartAssembly, overhead(limits))
            .map_err(ReportFailure::Capacity)
    };
    let reservation = reserve().map_err(|reason| {
        Box::new(PartialReport {
            key: key.clone(),
            progress: ReportProgress::default(),
            reason,
        })
    })?;
    Ok(CollectedReport {
        key,
        progress: ReportProgress::default(),
        items: Vec::with_capacity(limits.maximum_items),
        reservation,
    })
}

fn overhead(limits: ReportLimits) -> usize {
    limits.maximum_items * size_of::<Box<[u8]>>() + size_of::<CollectedReport>() + 4 * 128
}

fn append(
    report: &mut CollectedReport,
    fragment: &ReportFragment,
    limits: ReportLimits,
    budget: &RuntimeResourceBudget,
) -> Result<bool, ReportFailure> {
    if fragment.key != report.key {
        return Err(ReportFailure::CorrelationMismatch);
    }
    if fragment.sequence < report.progress.fragments {
        return Err(ReportFailure::DuplicateOrConflictingSequence);
    }
    if fragment.sequence != report.progress.fragments {
        return Err(ReportFailure::MissingOrOutOfOrderSequence);
    }
    if report.progress.fragments >= limits.maximum_fragments
        || (fragment.more && report.progress.fragments + 1 == limits.maximum_fragments)
    {
        return Err(ReportFailure::FragmentLimit);
    }
    let items = report
        .progress
        .items
        .checked_add(fragment.items.len())
        .filter(|count| *count <= limits.maximum_items)
        .ok_or(ReportFailure::ItemLimit)?;
    let fragment_bytes = fragment
        .items
        .iter()
        .try_fold(0usize, |bytes, item| bytes.checked_add(item.len()))
        .ok_or(ReportFailure::ByteLimit)?;
    if fragment_bytes > budget.limits().maximum_ocpp_message_bytes {
        return Err(ReportFailure::ByteLimit);
    }
    let bytes = report
        .progress
        .bytes
        .checked_add(fragment_bytes)
        .filter(|bytes| *bytes <= limits.maximum_bytes)
        .ok_or(ReportFailure::ByteLimit)?;
    report
        .reservation
        .try_resize(overhead(limits) + bytes)
        .map_err(ReportFailure::Capacity)?;
    // Copy only admitted lengths, discarding potentially oversized input capacities.
    report
        .items
        .extend(fragment.items.iter().map(|item| item.as_slice().into()));
    report.progress = ReportProgress {
        fragments: report.progress.fragments + 1,
        items,
        bytes,
    };
    Ok(!fragment.more)
}
