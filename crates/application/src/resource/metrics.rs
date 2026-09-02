/// Current item counts for every bounded daemon queue class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeQueueSnapshot {
    pub charger_requests: usize,
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
