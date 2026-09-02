use serde_json::{Map, Value, json};
use uob_application::{
    AuxiliaryProcess, ComponentHealthState, ComponentKind, CoreLoopState, HealthSnapshot,
    ReadinessState, StorageHealthState,
};

pub(super) fn health_json(snapshot: &HealthSnapshot) -> Value {
    let resources = snapshot.runtime;
    let queues = resources.queues;
    let limits = snapshot.runtime_limits;
    json!({
        "readiness": readiness(snapshot.readiness),
        "core_loop": core_loop(snapshot.core_loop),
        "storage": storage(snapshot.storage),
        "accepts_new_sessions": snapshot.accepts_new_sessions,
        "storage_retention": snapshot.storage_retention.map(|value| json!({
            "budget_bytes": value.budget_bytes,
            "active_session_reserve_bytes": value.active_session_reserve_bytes,
            "used_bytes": value.used_bytes,
            "retained_critical_events": value.retained_critical_events,
            "retained_required_deliveries": value.retained_required_deliveries,
            "dropped_best_effort_telemetry": value.dropped_best_effort_telemetry,
            "dropped_best_effort_deliveries": value.dropped_best_effort_deliveries,
            "pruned_expired_events": value.pruned_expired_events,
            "pruned_expired_delivery_attempts": value.pruned_expired_delivery_attempts,
        })),
        "runtime": {
            "connected_stations": resources.connected_stations,
            "queued_payload_bytes": resources.queued_payload_bytes,
            "trace_ring_bytes": resources.trace_ring_bytes,
            "dropped_diagnostics": resources.dropped_diagnostics,
            "queues": {
                "charger_requests": queues.charger_requests,
                "database_work": queues.database_work,
                "subscribers": queues.subscribers,
                "pending_requests": queues.pending_requests,
                "multipart_assemblies": queues.multipart_assemblies,
                "target_ingress": queues.target_ingress,
                "target_egress": queues.target_egress,
                "target_retries": queues.target_retries,
                "critical_reports": queues.critical_reports,
                "diagnostics": queues.diagnostics,
                "exporter_batches": queues.exporter_batches,
                "capture_records": queues.capture_records,
            }
        },
        "runtime_limits": {
            "maximum_connected_stations": limits.maximum_connected_stations,
            "maximum_ocpp_message_bytes": limits.maximum_ocpp_message_bytes,
            "aggregate_queued_payload_bytes": limits.aggregate_queued_payload_bytes,
            "reserved_critical_payload_bytes": limits.reserved_critical_payload_bytes,
            "trace_ring_bytes": limits.trace_ring_bytes,
            "queues": {
                "database_work": limits.queues.database_work,
                "subscribers": limits.queues.subscribers,
                "pending_requests": limits.queues.pending_requests,
                "multipart_assemblies": limits.queues.multipart_assemblies,
                "target_ingress": limits.queues.target_ingress,
                "target_egress": limits.queues.target_egress,
                "target_retries": limits.queues.target_retries,
                "critical_reports": limits.queues.critical_reports,
                "diagnostics": limits.queues.diagnostics,
                "exporter_batches": limits.queues.exporter_batches,
                "capture_records": limits.queues.capture_records,
            }
        },
        "local_response_latency": latency(snapshot.local_response_latency),
        "storage_latency": latency(snapshot.storage_latency),
        "uncertain_commands": snapshot.uncertain_commands,
        "dropped_telemetry": snapshot.dropped_telemetry,
        "components": components(snapshot),
        "daemon_process": snapshot.daemon_process.map(process),
        "auxiliary_processes": auxiliary_processes(snapshot),
    })
}

fn components(snapshot: &HealthSnapshot) -> Value {
    let mut values = Map::new();
    for (kind, health) in &snapshot.components {
        values.insert(
            component(*kind).into(),
            json!({
                "state": component_state(health.state),
                "reconnects": health.reconnects,
                "backlog_items": health.backlog_items,
                "in_flight_items": health.in_flight_items,
                "active_connections": health.active_connections,
                "reason": health.reason,
            }),
        );
    }
    Value::Object(values)
}

fn auxiliary_processes(snapshot: &HealthSnapshot) -> Value {
    let mut values = Map::new();
    for (kind, metrics) in &snapshot.auxiliary_processes {
        values.insert(auxiliary(*kind).into(), process(*metrics));
    }
    Value::Object(values)
}

fn latency(value: uob_application::LatencySnapshot) -> Value {
    json!({
        "samples": value.samples,
        "p95_upper_bound_ms": value.p95_upper_bound_ms,
        "maximum_ms": value.maximum_ms,
        "overflow_samples": value.overflow_samples,
    })
}

fn process(value: uob_application::ProcessResourceMetrics) -> Value {
    json!({
        "rss_bytes": value.rss_bytes,
        "cpu_time_milliseconds": value.cpu_time_milliseconds,
    })
}

const fn readiness(value: ReadinessState) -> &'static str {
    match value {
        ReadinessState::Ready => "ready",
        ReadinessState::NotReady => "not_ready",
    }
}

const fn core_loop(value: CoreLoopState) -> &'static str {
    match value {
        CoreLoopState::Starting => "starting",
        CoreLoopState::Ready => "ready",
        CoreLoopState::Failed => "failed",
    }
}

const fn storage(value: StorageHealthState) -> &'static str {
    match value {
        StorageHealthState::Starting => "starting",
        StorageHealthState::Safe => "safe",
        StorageHealthState::CapacityProtected => "capacity_protected",
        StorageHealthState::Failed => "failed",
    }
}

const fn component(value: ComponentKind) -> &'static str {
    match value {
        ComponentKind::Target => "target",
        ComponentKind::ExternalBroker => "external_broker",
        ComponentKind::ExternalClient => "external_client",
        ComponentKind::ExternalExporter => "external_exporter",
    }
}

const fn component_state(value: ComponentHealthState) -> &'static str {
    match value {
        ComponentHealthState::Disabled => "disabled",
        ComponentHealthState::Ready => "ready",
        ComponentHealthState::Degraded => "degraded",
        ComponentHealthState::Reconnecting => "reconnecting",
        ComponentHealthState::Stopped => "stopped",
    }
}

const fn auxiliary(value: AuxiliaryProcess) -> &'static str {
    match value {
        AuxiliaryProcess::Broker => "broker",
        AuxiliaryProcess::ExternalDatabase => "external_database",
        AuxiliaryProcess::Browser => "browser",
        AuxiliaryProcess::Simulator => "simulator",
        AuxiliaryProcess::Provider => "provider",
        AuxiliaryProcess::ReleaseManager => "release_manager",
    }
}
