use std::sync::Arc;

use super::{
    AdmissionError, AdmissionLimit, Inner, RuntimeResourceLimits, Usage, WorkClass, item_limit,
    lock_usage,
};

pub(super) fn reserve(
    inner: &Arc<Inner>,
    class: WorkClass,
    bytes: usize,
) -> Result<(), AdmissionError> {
    let mut usage = lock_usage(inner);
    reserve_with_usage(inner.limits, &mut usage, class, bytes)
}

pub(super) fn reserve_with_usage(
    limits: RuntimeResourceLimits,
    usage: &mut Usage,
    class: WorkClass,
    bytes: usize,
) -> Result<(), AdmissionError> {
    let maximum_items = item_limit(limits, class);
    if usage.items[class.index()] >= maximum_items {
        return Err(AdmissionError {
            limit: AdmissionLimit::QueueItems(class),
            maximum: maximum_items,
            requested: usage.items[class.index()].saturating_add(1),
        });
    }
    admit_bytes(limits, usage, class, bytes)?;
    usage.items[class.index()] += 1;
    usage.queued_payload_bytes += bytes;
    if class == WorkClass::CaptureTrace {
        usage.trace_ring_bytes += bytes;
    }
    Ok(())
}

pub(super) fn grow(
    inner: &Arc<Inner>,
    class: WorkClass,
    bytes: usize,
) -> Result<(), AdmissionError> {
    let mut usage = lock_usage(inner);
    admit_bytes(inner.limits, &usage, class, bytes)?;
    usage.queued_payload_bytes += bytes;
    if class == WorkClass::CaptureTrace {
        usage.trace_ring_bytes += bytes;
    }
    Ok(())
}

fn admit_bytes(
    limits: RuntimeResourceLimits,
    usage: &Usage,
    class: WorkClass,
    bytes: usize,
) -> Result<(), AdmissionError> {
    if class == WorkClass::CaptureTrace
        && usage.trace_ring_bytes.saturating_add(bytes) > limits.trace_ring_bytes
    {
        return Err(AdmissionError {
            limit: AdmissionLimit::TraceRingBytes,
            maximum: limits.trace_ring_bytes,
            requested: usage.trace_ring_bytes.saturating_add(bytes),
        });
    }
    let maximum = if class.is_critical() {
        limits.aggregate_queued_payload_bytes
    } else {
        limits
            .aggregate_queued_payload_bytes
            .saturating_sub(limits.reserved_critical_payload_bytes)
    };
    let requested = usage.queued_payload_bytes.checked_add(bytes);
    if requested.is_none_or(|requested| requested > maximum) {
        return Err(AdmissionError {
            limit: AdmissionLimit::AggregatePayloadBytes,
            maximum,
            requested: requested.unwrap_or(usize::MAX),
        });
    }
    Ok(())
}

pub(super) fn shrink(inner: &Arc<Inner>, class: WorkClass, bytes: usize) {
    let mut usage = lock_usage(inner);
    usage.queued_payload_bytes = usage.queued_payload_bytes.saturating_sub(bytes);
    if class == WorkClass::CaptureTrace {
        usage.trace_ring_bytes = usage.trace_ring_bytes.saturating_sub(bytes);
    }
}

pub(super) fn release(inner: &Arc<Inner>, class: WorkClass, bytes: usize) {
    if matches!(class, WorkClass::Diagnostic | WorkClass::CaptureTrace) {
        inner.diagnostics.release(class, bytes);
        return;
    }
    let mut usage = lock_usage(inner);
    usage.items[class.index()] = usage.items[class.index()].saturating_sub(1);
    usage.queued_payload_bytes = usage.queued_payload_bytes.saturating_sub(bytes);
}
