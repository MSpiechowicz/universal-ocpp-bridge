//! Best-effort process-scoped instrumentation. No raw payloads or wall-clock latency arithmetic.
use crate::{
    CommandClock, DiagnosticAttribute, DiagnosticBoundary, DiagnosticObservation,
    DiagnosticOutcome, DiagnosticSummary, DiagnosticTraceContext, SafeDiagnosticField,
    SanitizedDiagnostic,
    capture::{CaptureFilter, CaptureLevel, CaptureManager},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Instant,
};
use uob_contracts::{
    BridgeId, CorrelationId, ProcessInstanceId, ProtocolEdition, StationId, TargetInstanceId,
    TargetKind, TraceDirection, TraceId, TraceSequence, TraceStage, UtcTimestamp,
};

/// Closed evidence stages. Success describes this stage only, never physical charging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowStage {
    OcppReceive,
    Validation,
    Application,
    DurableCommit,
    CommandIngress,
    Authorization,
    Deduplication,
    CommandDispatch,
    ProtocolResponse,
    ObservedEffect,
    OcppSend,
    TargetMapping,
    TargetEnqueue,
    TargetReport,
    ManagementDelivery,
    StateChange,
}
impl FlowStage {
    const fn name(self) -> &'static str {
        match self {
            Self::OcppReceive => "ocpp.receive",
            Self::Validation => "validation",
            Self::Application => "application",
            Self::DurableCommit => "storage.commit",
            Self::CommandIngress => "command.ingress",
            Self::Authorization => "command.authorization",
            Self::Deduplication => "command.deduplication",
            Self::CommandDispatch => "command.dispatch",
            Self::ProtocolResponse => "command.protocol_response",
            Self::ObservedEffect => "command.observed_effect",
            Self::OcppSend => "ocpp.send",
            Self::TargetMapping => "target.mapping",
            Self::TargetEnqueue => "target.enqueue",
            Self::TargetReport => "target.report",
            Self::ManagementDelivery => "management.delivery",
            Self::StateChange => "state.changed_fields",
        }
    }
    const fn direction(self) -> TraceDirection {
        match self {
            Self::OcppReceive | Self::CommandIngress => TraceDirection::Inbound,
            Self::OcppSend | Self::TargetEnqueue | Self::ManagementDelivery => {
                TraceDirection::Outbound
            }
            _ => TraceDirection::Internal,
        }
    }
}

/// Closed safe evidence labels; arbitrary error/payload text cannot enter this API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowEvidence {
    Completed,
    Rejected,
    Duplicate,
    Uncertain,
    NotTransmitted,
    Accepted,
    LocallyExposed,
    PeerAcknowledged,
    Observed,
    Uncorrelated,
    Failed,
    Stale,
}
impl FlowEvidence {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Rejected => "rejected",
            Self::Duplicate => "duplicate",
            Self::Uncertain => "uncertain",
            Self::NotTransmitted => "not_transmitted",
            Self::Accepted => "charger_accepted",
            Self::LocallyExposed => "locally_exposed",
            Self::PeerAcknowledged => "peer_acknowledged",
            Self::Observed => "observed_effect",
            Self::Uncorrelated => "uncorrelated",
            Self::Failed => "failed",
            Self::Stale => "stale",
        }
    }
}

struct Shared {
    process: ProcessInstanceId,
    bridge: BridgeId,
    capture: CaptureManager,
    clock: Arc<dyn CommandClock>,
    sender: Option<SyncSender<SanitizedDiagnostic>>,
    sequence: AtomicU64,
    dropped: AtomicU64,
}
/// Clone one emitter throughout a process. Disabled by default; capture policy is checked per stage.
#[derive(Clone, Default)]
pub struct FlowDiagnostics(Option<Arc<Shared>>);
impl std::fmt::Debug for FlowDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowDiagnostics").finish_non_exhaustive()
    }
}
impl FlowDiagnostics {
    /// Creates a bounded transient sink for the host. The receiver is not a durable replay stream.
    /// # Errors
    /// Rejects capacities outside 1..=128, bounding queued diagnostics to at most 8 MiB.
    pub fn channel(
        process: ProcessInstanceId,
        bridge: BridgeId,
        capture: CaptureManager,
        clock: Arc<dyn CommandClock>,
        capacity: usize,
    ) -> Result<(Self, Receiver<SanitizedDiagnostic>), &'static str> {
        if !(1..=128).contains(&capacity) {
            return Err("invalid trace queue capacity");
        }
        let (sender, receiver) = mpsc::sync_channel(capacity);
        Ok((
            Self(Some(Arc::new(Shared {
                process,
                bridge,
                capture,
                clock,
                sender: Some(sender),
                sequence: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
            }))),
            receiver,
        ))
    }
    /// Sends directly into the service's single capture-owned memory ring.
    /// Producer lock contention sheds diagnostics; no task or durable store is created.
    #[must_use]
    pub fn retained(
        process: ProcessInstanceId,
        bridge: BridgeId,
        capture: CaptureManager,
        clock: Arc<dyn CommandClock>,
    ) -> Self {
        Self(Some(Arc::new(Shared {
            process,
            bridge,
            capture,
            clock,
            sender: None,
            sequence: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        })))
    }
    /// Starts metadata-only context; source time is never used to calculate elapsed duration.
    #[must_use]
    pub fn span(
        &self,
        correlation: Option<CorrelationId>,
        station: Option<StationId>,
        protocol: Option<ProtocolEdition>,
    ) -> FlowSpan {
        FlowSpan {
            diagnostics: self.clone(),
            correlation,
            station,
            protocol,
            target: None,
            start: Instant::now(),
        }
    }
    /// Number of records shed because serialization, bounds, or the nonblocking sink rejected them.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.0.as_ref().map_or(0, |s| {
            if s.sender.is_none() {
                s.capture.dropped()
            } else {
                s.dropped.load(Ordering::Relaxed)
            }
        })
    }
}
/// Small owned context carried through queues and awaits; no retained per-request lookup table.
#[derive(Clone, Debug)]
pub struct FlowSpan {
    diagnostics: FlowDiagnostics,
    correlation: Option<CorrelationId>,
    station: Option<StationId>,
    protocol: Option<ProtocolEdition>,
    target: Option<(TargetInstanceId, TargetKind)>,
    start: Instant,
}
impl FlowSpan {
    /// Attaches an exact configured destination, never an endpoint or inferred target.
    #[must_use]
    pub fn with_target(mut self, id: TargetInstanceId, kind: TargetKind) -> Self {
        self.target = Some((id, kind));
        self
    }
    /// Emits a closed safe stage after rechecking active capture filters.
    pub fn emit(&self, stage: FlowStage, evidence: FlowEvidence) {
        self.emit_fields(stage, evidence, Vec::new());
    }
    /// Adds typed, bounded safe fields. Only the first 16 attributes are inspected.
    /// # Panics
    /// Only if a compile-time stage or a nonempty generated trace identity becomes invalid.
    pub fn emit_fields(
        &self,
        stage: FlowStage,
        evidence: FlowEvidence,
        fields: Vec<SafeDiagnosticField>,
    ) {
        let Some(shared) = &self.diagnostics.0 else {
            return;
        };
        let filter = CaptureFilter {
            bridge: shared.bridge.clone(),
            station: self.station.clone(),
            target: self.target.as_ref().map(|(id, _)| id.clone()),
        };
        if shared.sender.is_none() {
            shared
                .capture
                .try_record(&filter, |sequence, shed_details| {
                    self.record(shared, stage, evidence, fields, sequence, shed_details)
                });
            return;
        }
        if !shared.capture.try_accepts(&filter, CaptureLevel::Metadata) {
            return;
        }
        let sequence = shared.sequence.fetch_add(1, Ordering::Relaxed);
        if let Some(record) = self.record(shared, stage, evidence, fields, sequence, false)
            && shared
                .sender
                .as_ref()
                .is_some_and(|sender| sender.try_send(record).is_ok())
        {
            return;
        }
        shared.dropped.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        shared: &Shared,
        stage: FlowStage,
        evidence: FlowEvidence,
        fields: Vec<SafeDiagnosticField>,
        sequence: u64,
        shed_details: bool,
    ) -> Option<SanitizedDiagnostic> {
        let oversized = shared.process.as_str().len() > 256
            || self
                .correlation
                .as_ref()
                .is_some_and(|s| s.as_str().len() > 1024)
            || self
                .station
                .as_ref()
                .is_some_and(|s| s.as_str().len() > 256)
            || self
                .target
                .as_ref()
                .is_some_and(|(id, kind)| id.as_str().len() > 256 || kind.as_str().len() > 256)
            || fields.iter().take(16).any(|field| match field {
                SafeDiagnosticField::Action(v) => v.as_str().len() > 256,
                SafeDiagnosticField::Station(v) => v.as_str().len() > 256,
                SafeDiagnosticField::Correlation(v) => v.as_str().len() > 1024,
                _ => false,
            });
        if oversized {
            return None;
        }
        let mut attributes = vec![DiagnosticAttribute::Safe(SafeDiagnosticField::Evidence(
            evidence,
        ))];
        if self.correlation.is_none() {
            attributes.push(DiagnosticAttribute::Safe(
                SafeDiagnosticField::CorrelationMissing,
            ));
        }
        if let Some(station) = &self.station {
            attributes.push(DiagnosticAttribute::Safe(SafeDiagnosticField::Station(
                station.clone(),
            )));
        }
        if let Some(protocol) = self.protocol {
            attributes.push(DiagnosticAttribute::Safe(SafeDiagnosticField::Protocol(
                protocol,
            )));
        }
        if shed_details {
            attributes.push(DiagnosticAttribute::Safe(
                SafeDiagnosticField::StateDetailsOmitted,
            ));
        } else {
            attributes.extend(fields.into_iter().take(16).map(DiagnosticAttribute::Safe));
        }
        let outcome = match evidence {
            FlowEvidence::Rejected | FlowEvidence::Failed | FlowEvidence::NotTransmitted => {
                DiagnosticOutcome::PolicyDenied
            }
            FlowEvidence::Uncertain => DiagnosticOutcome::Uncertain,
            _ => DiagnosticOutcome::Succeeded,
        };
        let context = DiagnosticTraceContext {
            trace_id: TraceId::new(format!("{}:{sequence}", shared.process.as_str()))
                .expect("trace ID"),
            process_instance_id: shared.process.clone(),
            trace_sequence: TraceSequence(sequence),
            target: self.target.clone(),
            correlation_id: self.correlation.clone(),
            parent_trace_id: None,
            stage: TraceStage::new(stage.name()).expect("closed stage"),
            direction: stage.direction(),
            observed_at: shared.clock.now(),
            duration_micros: Some(
                u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX),
            ),
            outcome,
        };
        let record = DiagnosticBoundary.serialize(
            context,
            DiagnosticObservation {
                summary: DiagnosticSummary::OperationCompleted,
                attributes,
            },
        );
        record
            .ok()
            .filter(|record| record.encoded_json().len() <= 64 * 1024)
    }
    /// Device timestamp remains explicitly named source time in safe details.
    pub fn source_time(&self, stage: FlowStage, time: UtcTimestamp) {
        self.emit_fields(
            stage,
            FlowEvidence::Completed,
            vec![SafeDiagnosticField::SourceTime(time)],
        );
    }
}
