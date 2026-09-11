use super::super::{format, stream};
use crate::{ManagementCaptureAuthenticator, ManagementCaptureConfiguration, capture_router};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request},
    response::Response,
};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;
use uob_application::{
    DiagnosticAttribute, DiagnosticBoundary, DiagnosticObservation, DiagnosticOutcome,
    DiagnosticSummary, DiagnosticTraceContext, RuntimeResourceBudget, RuntimeResourceLimits,
    SafeDiagnosticField, SensitiveDataClass, SensitiveDiagnosticValue, UnknownVendorPayload,
    capture::{CaptureFilter, CaptureGrant, CaptureLevel, CaptureManager, CapturePermission},
};
use uob_contracts::*;

pub struct Authentication {
    pub mode: AtomicUsize,
}
impl ManagementCaptureAuthenticator for Authentication {
    fn authenticate(&self, token: &str) -> Option<CaptureGrant> {
        let mode = self.mode.load(Ordering::SeqCst);
        match token {
            "reader" if mode == 0 => Some(grant("a", "target", true)),
            "reader" if mode == 1 => None,
            "reader" | "wrong-station" if mode == 2 || token == "wrong-station" => {
                Some(grant("b", "target", true))
            }
            "reader" | "wrong-target" => Some(grant("a", "elsewhere", true)),
            "control" => Some(grant("a", "target", false)),
            _ => None,
        }
    }
}
pub fn grant(station: &str, target: &str, read: bool) -> CaptureGrant {
    let mut permissions = vec![CapturePermission::Capture];
    if read {
        permissions.push(CapturePermission::Read);
    }
    CaptureGrant::new(
        BridgeId::new("bridge").unwrap(),
        permissions,
        Some(vec![StationId::new(station).unwrap()]),
        Some(vec![TargetInstanceId::new(target).unwrap()]),
    )
    .unwrap()
}
pub fn filter() -> CaptureFilter {
    CaptureFilter {
        bridge: BridgeId::new("bridge").unwrap(),
        station: Some(StationId::new("a").unwrap()),
        target: Some(TargetInstanceId::new("target").unwrap()),
    }
}
pub fn identity() -> ServiceIdentity {
    ServiceIdentity {
        bridge_id: filter().bridge,
        runtime: RuntimeIdentity {
            environment: Environment::Production,
            release_id: ReleaseId::new("release").unwrap(),
            release_digest: ArtifactDigest::new("sha256:release").unwrap(),
            process_instance_id: ProcessInstanceId::new("process").unwrap(),
        },
        selected_target_id: filter().target,
    }
}
pub struct Setup {
    pub manager: CaptureManager,
    pub resources: RuntimeResourceBudget,
    pub auth: Arc<Authentication>,
    pub router: Router,
    pub id: u64,
}
impl Setup {
    pub fn new(records: usize) -> Self {
        let mut limits = RuntimeResourceLimits::default();
        limits.queues.capture_records = records;
        let resources = RuntimeResourceBudget::new(limits).unwrap();
        let manager = CaptureManager::with_resources(true, resources.clone());
        let id = manager
            .start(
                &grant("a", "target", true),
                filter(),
                CaptureLevel::RedactedPayload,
                None,
            )
            .unwrap()
            .id;
        let auth = Arc::new(Authentication {
            mode: AtomicUsize::new(0),
        });
        let configuration = ManagementCaptureConfiguration {
            manager: manager.clone(),
            authenticator: auth.clone(),
        };
        let router = capture_router(identity(), configuration);
        Self {
            manager,
            resources,
            auth,
            router,
            id,
        }
    }
    pub fn configuration(&self) -> ManagementCaptureConfiguration {
        ManagementCaptureConfiguration {
            manager: self.manager.clone(),
            authenticator: self.auth.clone(),
        }
    }
    pub fn emit(&self) {
        assert!(self.manager.try_record(&filter(), |sequence, _| {
            Some(
                DiagnosticBoundary
                    .serialize(
                        DiagnosticTraceContext {
                            trace_id: TraceId::new(format!("trace-{sequence}")).unwrap(),
                            process_instance_id: identity().runtime.process_instance_id,
                            trace_sequence: TraceSequence(sequence),
                            target: Some((
                                filter().target.unwrap(),
                                TargetKind::new("mqtt").unwrap(),
                            )),
                            correlation_id: None,
                            parent_trace_id: None,
                            stage: TraceStage::new("application").unwrap(),
                            direction: TraceDirection::Internal,
                            observed_at: serde_json::from_str("\"2026-09-11T00:00:00Z\"").unwrap(),
                            duration_micros: None,
                            outcome: DiagnosticOutcome::Succeeded,
                        },
                        DiagnosticObservation {
                            summary: DiagnosticSummary::PayloadObserved,
                            attributes: vec![
                                DiagnosticAttribute::Safe(SafeDiagnosticField::Station(
                                    filter().station.unwrap(),
                                )),
                                DiagnosticAttribute::Sensitive(SensitiveDiagnosticValue::new(
                                    SensitiveDataClass::AuthorizationToken,
                                    "secret-token",
                                )),
                                DiagnosticAttribute::Sensitive(SensitiveDiagnosticValue::new(
                                    SensitiveDataClass::EndpointSecret,
                                    "password=secret",
                                )),
                                DiagnosticAttribute::UnknownVendorPayload(
                                    UnknownVendorPayload::new(b"vendor-secret".to_vec()),
                                ),
                            ],
                        },
                    )
                    .unwrap(),
            )
        }));
    }
    pub fn stop(&self) {
        self.manager
            .stop(&grant("a", "target", true), self.id)
            .unwrap();
    }
    pub async fn download(&self) -> Response {
        self.router
            .clone()
            .oneshot(request("reader", "process", self.id))
            .await
            .unwrap()
    }
    pub fn with_limits(&self, limits: stream::Limits) -> Response {
        let grant = grant("a", "target", true);
        let status = self.manager.status(&grant).unwrap();
        let lease = self.manager.lease(&grant, self.id, true).unwrap();
        let window = lease.read_after(None).unwrap().window;
        let manifest = format::manifest(&identity(), &status, window, limits).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer reader".parse().unwrap());
        stream::response(
            self.configuration(),
            headers,
            self.id,
            lease,
            window,
            manifest,
            limits,
        )
    }
}
pub fn request(token: &str, process: &str, id: u64) -> Request<Body> {
    Request::builder()
        .uri(format!("/api/v1/diagnostics/capture/{process}/{id}/export"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}
pub fn lines(text: &str) -> Vec<Value> {
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    validate_lines(&lines);
    lines
}
pub async fn body(response: Response) -> Vec<Value> {
    let bytes = to_bytes(response.into_body(), 100_000).await.unwrap();
    lines(std::str::from_utf8(&bytes).unwrap())
}

pub fn validate_lines(lines: &[Value]) {
    let schema: Value =
        serde_json::from_str(include_str!("../../../schemas/capture-line-v1.schema.json")).unwrap();
    let identity: Value = serde_json::from_str(include_str!(
        "../../../../../crates/contracts/schemas/v1.0/service-identity.schema.json"
    ))
    .unwrap();
    let trace: Value = serde_json::from_str(include_str!(
        "../../../../../crates/contracts/schemas/v1.0/trace-record.schema.json"
    ))
    .unwrap();
    let registry = jsonschema::Registry::new()
        .add(
            "https://schemas.universal-ocpp-bridge.dev/contracts/v1.0/service-identity.schema.json",
            identity,
        )
        .unwrap()
        .add(
            "https://schemas.universal-ocpp-bridge.dev/contracts/v1.0/trace-record.schema.json",
            trace,
        )
        .unwrap()
        .prepare()
        .unwrap();
    let validator = jsonschema::options()
        .offline()
        .with_registry(&registry)
        .build(&schema)
        .unwrap();
    for line in lines {
        assert!(
            validator.is_valid(line),
            "capture line schema mismatch: {:?}",
            validator
                .iter_errors(line)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
    }
}
