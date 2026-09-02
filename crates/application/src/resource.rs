use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

mod metrics;
mod policy;

pub use metrics::RuntimeQueueSnapshot;
pub use policy::{
    LaggingConsumer, LaggingConsumerAction, ReplaceableTelemetrySlot, TelemetryReplaceOutcome,
};

/// Default number of simultaneously connected stations.
pub const DEFAULT_MAX_CONNECTED_STATIONS: usize = 16;
/// Default maximum encoded OCPP message size.
pub const DEFAULT_MAX_OCPP_MESSAGE_BYTES: usize = 256 * 1024;
/// Default shared byte cap across every in-memory work queue.
pub const DEFAULT_AGGREGATE_QUEUED_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
/// Default maximum memory reserved for best-effort trace capture.
pub const DEFAULT_TRACE_RING_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_CRITICAL_RESERVE_BYTES: usize = 512 * 1024;

/// Per-workload item-count limits applied in addition to shared byte accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeQueueLimits {
    pub database_work: usize,
    pub subscribers: usize,
    pub pending_requests: usize,
    pub multipart_assemblies: usize,
    pub target_ingress: usize,
    pub target_egress: usize,
    pub target_retries: usize,
    pub critical_reports: usize,
    pub diagnostics: usize,
    pub exporter_batches: usize,
    pub capture_records: usize,
}

impl Default for RuntimeQueueLimits {
    fn default() -> Self {
        Self {
            database_work: 64,
            subscribers: 16,
            pending_requests: 64,
            multipart_assemblies: 4,
            target_ingress: 64,
            target_egress: 64,
            target_retries: 64,
            critical_reports: 64,
            diagnostics: 128,
            exporter_batches: 16,
            capture_records: 2_000,
        }
    }
}

/// Process-wide runtime limits validated once by the composition root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeResourceLimits {
    pub maximum_connected_stations: usize,
    pub maximum_ocpp_message_bytes: usize,
    pub aggregate_queued_payload_bytes: usize,
    pub reserved_critical_payload_bytes: usize,
    pub trace_ring_bytes: usize,
    pub queues: RuntimeQueueLimits,
}

impl Default for RuntimeResourceLimits {
    fn default() -> Self {
        Self {
            maximum_connected_stations: DEFAULT_MAX_CONNECTED_STATIONS,
            maximum_ocpp_message_bytes: DEFAULT_MAX_OCPP_MESSAGE_BYTES,
            aggregate_queued_payload_bytes: DEFAULT_AGGREGATE_QUEUED_PAYLOAD_BYTES,
            reserved_critical_payload_bytes: DEFAULT_CRITICAL_RESERVE_BYTES,
            trace_ring_bytes: DEFAULT_TRACE_RING_BYTES,
            queues: RuntimeQueueLimits::default(),
        }
    }
}

/// Queue or admission boundary that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionLimit {
    Configuration,
    ConnectedStations,
    OcppMessageBytes,
    AggregatePayloadBytes,
    QueueItems(WorkClass),
    TraceRingBytes,
}

/// Predictable, payload-free capacity error returned before work is queued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionError {
    pub limit: AdmissionLimit,
    pub maximum: usize,
    pub requested: usize,
}

/// Shared work classes that participate in process-wide byte accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkClass {
    ChargerRequest,
    DatabaseWork,
    Subscriber,
    PendingRequest,
    MultipartAssembly,
    TargetIngress,
    TargetEgress,
    TargetRetry,
    CriticalReport,
    Diagnostic,
    ExporterBatch,
    CaptureTrace,
}

impl WorkClass {
    const COUNT: usize = 12;

    const fn index(self) -> usize {
        match self {
            Self::ChargerRequest => 0,
            Self::DatabaseWork => 1,
            Self::Subscriber => 2,
            Self::PendingRequest => 3,
            Self::MultipartAssembly => 4,
            Self::TargetIngress => 5,
            Self::TargetEgress => 6,
            Self::TargetRetry => 7,
            Self::CriticalReport => 8,
            Self::Diagnostic => 9,
            Self::ExporterBatch => 10,
            Self::CaptureTrace => 11,
        }
    }

    const fn is_critical(self) -> bool {
        matches!(self, Self::ChargerRequest | Self::CriticalReport)
    }
}

#[derive(Debug)]
struct Usage {
    connected_stations: usize,
    queued_payload_bytes: usize,
    trace_ring_bytes: usize,
    items: [usize; WorkClass::COUNT],
    dropped_diagnostics: u64,
}

/// Observable capacity counters suitable for sanitized health reporting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeResourceSnapshot {
    pub connected_stations: usize,
    pub queued_payload_bytes: usize,
    pub trace_ring_bytes: usize,
    pub dropped_diagnostics: u64,
    pub queues: RuntimeQueueSnapshot,
}

#[derive(Debug)]
struct Inner {
    limits: RuntimeResourceLimits,
    usage: Mutex<Usage>,
}

/// Cloneable process-wide admission authority shared by every producer.
#[derive(Clone, Debug)]
pub struct RuntimeResourceBudget {
    inner: Arc<Inner>,
}

impl RuntimeResourceBudget {
    /// Creates a shared budget after validating every bound.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when a limit is zero or reserved capacities conflict.
    pub fn new(limits: RuntimeResourceLimits) -> Result<Self, AdmissionError> {
        validate_limits(limits)?;
        Ok(Self {
            inner: Arc::new(Inner {
                limits,
                usage: Mutex::new(Usage {
                    connected_stations: 0,
                    queued_payload_bytes: 0,
                    trace_ring_bytes: 0,
                    items: [0; WorkClass::COUNT],
                    dropped_diagnostics: 0,
                }),
            }),
        })
    }

    /// Returns the immutable configured limits.
    #[must_use]
    pub fn limits(&self) -> &RuntimeResourceLimits {
        &self.inner.limits
    }

    /// Rejects an oversized protocol message before parsing or queueing it.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when `encoded_bytes` exceeds the configured message limit.
    pub fn validate_ocpp_message(&self, encoded_bytes: usize) -> Result<(), AdmissionError> {
        let maximum = self.inner.limits.maximum_ocpp_message_bytes;
        if encoded_bytes > maximum {
            return Err(AdmissionError {
                limit: AdmissionLimit::OcppMessageBytes,
                maximum,
                requested: encoded_bytes,
            });
        }
        Ok(())
    }

    /// Reserves one connected-station slot until the returned guard is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when all configured station slots are occupied.
    pub fn admit_station(&self) -> Result<StationAdmission, AdmissionError> {
        let mut usage = lock_usage(&self.inner);
        let maximum = self.inner.limits.maximum_connected_stations;
        if usage.connected_stations >= maximum {
            return Err(AdmissionError {
                limit: AdmissionLimit::ConnectedStations,
                maximum,
                requested: usage.connected_stations.saturating_add(1),
            });
        }
        usage.connected_stations += 1;
        drop(usage);
        Ok(StationAdmission {
            inner: Some(Arc::clone(&self.inner)),
        })
    }

    /// Reserves one item and its encoded bytes across all runtime queues.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when the class item limit, shared byte cap, protected critical
    /// reserve, or trace-ring cap would be exceeded.
    pub fn try_reserve(
        &self,
        class: WorkClass,
        bytes: usize,
    ) -> Result<RuntimeReservation, AdmissionError> {
        reserve(&self.inner, class, bytes)?;
        Ok(RuntimeReservation {
            inner: Some(Arc::clone(&self.inner)),
            class,
            bytes,
        })
    }

    /// Records an intentionally shed best-effort diagnostic.
    pub fn record_diagnostic_drop(&self, _reason: DiagnosticDropReason) {
        let mut usage = lock_usage(&self.inner);
        usage.dropped_diagnostics = usage.dropped_diagnostics.saturating_add(1);
    }

    /// Returns current sanitized aggregate use.
    #[must_use]
    pub fn snapshot(&self) -> RuntimeResourceSnapshot {
        let usage = lock_usage(&self.inner);
        RuntimeResourceSnapshot {
            connected_stations: usage.connected_stations,
            queued_payload_bytes: usage.queued_payload_bytes,
            trace_ring_bytes: usage.trace_ring_bytes,
            dropped_diagnostics: usage.dropped_diagnostics,
            queues: RuntimeQueueSnapshot {
                charger_requests: usage.items[WorkClass::ChargerRequest.index()],
                database_work: usage.items[WorkClass::DatabaseWork.index()],
                subscribers: usage.items[WorkClass::Subscriber.index()],
                pending_requests: usage.items[WorkClass::PendingRequest.index()],
                multipart_assemblies: usage.items[WorkClass::MultipartAssembly.index()],
                target_ingress: usage.items[WorkClass::TargetIngress.index()],
                target_egress: usage.items[WorkClass::TargetEgress.index()],
                target_retries: usage.items[WorkClass::TargetRetry.index()],
                critical_reports: usage.items[WorkClass::CriticalReport.index()],
                diagnostics: usage.items[WorkClass::Diagnostic.index()],
                exporter_batches: usage.items[WorkClass::ExporterBatch.index()],
                capture_records: usage.items[WorkClass::CaptureTrace.index()],
            },
        }
    }
}

/// RAII guard for one connected station.
#[derive(Debug)]
pub struct StationAdmission {
    inner: Option<Arc<Inner>>,
}

impl Drop for StationAdmission {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        let mut usage = lock_usage(&inner);
        usage.connected_stations = usage.connected_stations.saturating_sub(1);
    }
}

/// RAII guard releasing queue bytes and item capacity on every exit path.
#[derive(Debug)]
pub struct RuntimeReservation {
    inner: Option<Arc<Inner>>,
    class: WorkClass,
    bytes: usize,
}

impl RuntimeReservation {
    /// Changes the reserved encoded size atomically, retaining the old size on failure.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when growing would exceed the applicable shared limit.
    pub fn try_resize(&mut self, new_bytes: usize) -> Result<(), AdmissionError> {
        let Some(inner) = self.inner.as_ref() else {
            return Err(configuration_error(new_bytes));
        };
        if new_bytes > self.bytes {
            grow(inner, self.class, new_bytes - self.bytes)?;
        } else {
            shrink(inner, self.class, self.bytes - new_bytes);
        }
        self.bytes = new_bytes;
        Ok(())
    }

    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for RuntimeReservation {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        release(&inner, self.class, self.bytes);
    }
}

/// Reason a diagnostic record was intentionally shed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticDropReason {
    Full,
    Disabled,
    Closed,
}

fn validate_limits(limits: RuntimeResourceLimits) -> Result<(), AdmissionError> {
    let queue_limits = [
        limits.queues.database_work,
        limits.queues.subscribers,
        limits.queues.pending_requests,
        limits.queues.multipart_assemblies,
        limits.queues.target_ingress,
        limits.queues.target_egress,
        limits.queues.target_retries,
        limits.queues.critical_reports,
        limits.queues.diagnostics,
        limits.queues.exporter_batches,
        limits.queues.capture_records,
    ];
    if limits.maximum_connected_stations == 0
        || limits.maximum_ocpp_message_bytes == 0
        || limits.aggregate_queued_payload_bytes == 0
        || limits.reserved_critical_payload_bytes > limits.aggregate_queued_payload_bytes
        || limits.trace_ring_bytes > limits.aggregate_queued_payload_bytes
        || queue_limits.contains(&0)
    {
        return Err(configuration_error(0));
    }
    Ok(())
}

pub(super) const fn configuration_error(requested: usize) -> AdmissionError {
    AdmissionError {
        limit: AdmissionLimit::Configuration,
        maximum: 0,
        requested,
    }
}

fn item_limit(limits: RuntimeResourceLimits, class: WorkClass) -> usize {
    match class {
        WorkClass::ChargerRequest | WorkClass::PendingRequest => limits.queues.pending_requests,
        WorkClass::DatabaseWork => limits.queues.database_work,
        WorkClass::Subscriber => limits.queues.subscribers,
        WorkClass::MultipartAssembly => limits.queues.multipart_assemblies,
        WorkClass::TargetIngress => limits.queues.target_ingress,
        WorkClass::TargetEgress => limits.queues.target_egress,
        WorkClass::TargetRetry => limits.queues.target_retries,
        WorkClass::CriticalReport => limits.queues.critical_reports,
        WorkClass::Diagnostic => limits.queues.diagnostics,
        WorkClass::ExporterBatch => limits.queues.exporter_batches,
        WorkClass::CaptureTrace => limits.queues.capture_records,
    }
}

fn reserve(inner: &Arc<Inner>, class: WorkClass, bytes: usize) -> Result<(), AdmissionError> {
    let mut usage = lock_usage(inner);
    let maximum_items = item_limit(inner.limits, class);
    if usage.items[class.index()] >= maximum_items {
        return Err(AdmissionError {
            limit: AdmissionLimit::QueueItems(class),
            maximum: maximum_items,
            requested: usage.items[class.index()].saturating_add(1),
        });
    }
    admit_bytes(inner.limits, &usage, class, bytes)?;
    usage.items[class.index()] += 1;
    usage.queued_payload_bytes += bytes;
    if class == WorkClass::CaptureTrace {
        usage.trace_ring_bytes += bytes;
    }
    Ok(())
}

fn grow(inner: &Arc<Inner>, class: WorkClass, bytes: usize) -> Result<(), AdmissionError> {
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
    let requested = usage.queued_payload_bytes.saturating_add(bytes);
    if requested > maximum {
        return Err(AdmissionError {
            limit: AdmissionLimit::AggregatePayloadBytes,
            maximum,
            requested,
        });
    }
    Ok(())
}

fn shrink(inner: &Arc<Inner>, class: WorkClass, bytes: usize) {
    let mut usage = lock_usage(inner);
    usage.queued_payload_bytes = usage.queued_payload_bytes.saturating_sub(bytes);
    if class == WorkClass::CaptureTrace {
        usage.trace_ring_bytes = usage.trace_ring_bytes.saturating_sub(bytes);
    }
}

fn release(inner: &Arc<Inner>, class: WorkClass, bytes: usize) {
    let mut usage = lock_usage(inner);
    usage.items[class.index()] = usage.items[class.index()].saturating_sub(1);
    usage.queued_payload_bytes = usage.queued_payload_bytes.saturating_sub(bytes);
    if class == WorkClass::CaptureTrace {
        usage.trace_ring_bytes = usage.trace_ring_bytes.saturating_sub(bytes);
    }
}

fn lock_usage(inner: &Inner) -> MutexGuard<'_, Usage> {
    inner.usage.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
