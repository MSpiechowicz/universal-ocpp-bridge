//! Monitoring replies and `NotifyMonitoringReport` delivery on the station's socket.
use super::{Stream, disconnect};
use crate::monitoring201::{ReportPlan201, transport};
use ocpp_client::TransportError;
use serde_json::{Value, json};
use std::time::Instant;

/// Fragments in flight share the sink's bounded tracking of unanswered station CALLs.
const TRACKED_CALLS: usize = 128;

impl Stream {
    /// The CALLRESULT payload for a monitoring CALL and, for an accepted report, its fragments.
    pub(super) async fn monitoring_reply(
        &mut self,
        action: &str,
        payload: &Value,
    ) -> (Value, Option<ReportPlan201>) {
        let mut sink = self.sink.lock().await;
        sink.reports.retain(|_, sent| sent.elapsed() < self.timeout);
        let registered = {
            let state = self.state.lock().expect("native state lock");
            state.socket_generation == self.generation && state.registered
        };
        let reply = transport::reply(action, payload, &self.state);
        let capacity = TRACKED_CALLS.saturating_sub(sink.reports.len());
        match reply.report {
            Some(plan) if !registered || plan.frames.len() > capacity => {
                (json!({"status": "Rejected"}), None)
            }
            report => (reply.payload, report),
        }
    }

    /// Send the fragments of one report. Returns true only after an actual closed socket.
    pub(super) async fn send_monitoring_report(
        &mut self,
        plan: &ReportPlan201,
    ) -> Result<bool, TransportError> {
        let mut sent = 0;
        for (index, frame) in plan.frames.iter().enumerate() {
            if plan.disconnect_after == Some(index) {
                disconnect(&self.state, self.generation);
                self.sink.lock().await.inner.close().await?;
                self.record_report(plan, sent, "Interrupted");
                return Ok(true);
            }
            let mut sink = self.sink.lock().await;
            let id = uuid::Uuid::new_v4().to_string();
            let call = json!([2, id, "NotifyMonitoringReport", frame]).to_string();
            if let Err(error) = sink.inner.send(call).await {
                drop(sink);
                self.record_report(plan, sent, "Failed");
                return Err(error);
            }
            sink.reports.insert(id, Instant::now());
            sent += 1;
        }
        let outcome = if plan.frames.len() < plan.planned {
            "Withheld"
        } else {
            "Complete"
        };
        self.record_report(plan, sent, outcome);
        Ok(false)
    }

    fn record_report(&self, plan: &ReportPlan201, sent: usize, outcome: &str) {
        if let Some(handle) = transport::handle(&self.state) {
            handle.report_finished(plan, sent, outcome);
        }
    }
}
