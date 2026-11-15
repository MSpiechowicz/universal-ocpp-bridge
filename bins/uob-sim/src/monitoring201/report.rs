//! `GetMonitoringReport` (use case N02): selecting monitors and cutting them into
//! `NotifyMonitoringReport` fragments, including the deliberate misbehaviour knobs.
use super::config::ReportFault201;
use super::events::timestamp;
use super::model::{Monitor, State, same_component, same_variable, selector_matches};
use super::request::ReportRequest;
use serde_json::{Value, json};
use time::OffsetDateTime;

/// The `NotifyMonitoringReport` calls a station sends for one accepted request.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportPlan201 {
    pub request_id: i32,
    /// `NotifyMonitoringReportRequest` payloads in the order they are sent.
    pub frames: Vec<Value>,
    /// Fragments of the complete report, which exceeds `frames` when a fault withholds some.
    pub planned: usize,
    /// Send the fragments before the `GetMonitoringReport` reply.
    pub before_reply: bool,
    /// Close the socket before sending the frame with this index.
    pub disconnect_after: Option<usize>,
}

impl State {
    /// The reply and, when accepted, the fragments that follow it.
    pub(super) fn monitoring_report(
        &self,
        request: &ReportRequest,
        now: OffsetDateTime,
    ) -> (Value, Option<ReportPlan201>) {
        // N02.FR.02: a criterion the station does not support.
        if request.criteria.as_ref().is_some_and(|criteria| {
            criteria
                .iter()
                .any(|criterion| !self.config.supported_criteria.contains(criterion))
        }) {
            return (json!({"status": "NotSupported"}), None);
        }
        let mut selected: Vec<&Monitor> = self
            .monitors
            .iter()
            .filter(|monitor| {
                request
                    .criteria
                    .as_ref()
                    .is_none_or(|criteria| criteria.contains(&monitor.kind.criterion()))
                    && request.selectors.as_ref().is_none_or(|selectors| {
                        selectors.iter().any(|(component, variable)| {
                            selector_matches(
                                (component, variable.as_ref()),
                                &monitor.component,
                                &monitor.variable,
                            )
                        })
                    })
            })
            .collect();
        // N02.FR.10: an empty result set.
        if selected.is_empty() {
            return (json!({"status": "EmptyResultSet"}), None);
        }
        selected.sort_by_key(|monitor| monitor.id);
        (
            json!({"status": "Accepted"}),
            Some(self.plan(request.request_id, &selected, now)),
        )
    }

    fn plan(&self, request_id: i32, selected: &[&Monitor], now: OffsetDateTime) -> ReportPlan201 {
        let per_fragment = usize::try_from(self.config.monitors_per_fragment).unwrap_or(1);
        let generated = timestamp(now);
        let fragments = selected.chunks(per_fragment).collect::<Vec<_>>();
        let planned = fragments.len();
        let fault = self.config.report_fault;
        let mut frames: Vec<Value> = fragments
            .iter()
            .enumerate()
            .map(|(number, monitors)| {
                // N02.FR.09: seqNo counts up from 0; the faults break that on purpose.
                let seq_no = match fault {
                    ReportFault201::SkipSeqNo if number > 0 => number + 1,
                    ReportFault201::DuplicateSeqNo if number > 0 => number - 1,
                    _ => number,
                };
                json!({"requestId": request_id, "seqNo": seq_no, "tbc": number + 1 < planned,
                    "generatedAt": generated, "monitor": group(monitors)})
            })
            .collect();
        if fault == ReportFault201::OmitFinalFragment {
            frames.pop();
        }
        ReportPlan201 {
            request_id,
            frames,
            planned,
            before_reply: fault == ReportFault201::FragmentsBeforeReply,
            disconnect_after: (fault == ReportFault201::DisconnectMidReport)
                .then(|| usize::from(planned > 1)),
        }
    }
}

/// Monitors of one component-variable share a `MonitoringDataType`.
fn group(monitors: &[&Monitor]) -> Vec<Value> {
    let mut groups: Vec<(&Monitor, Vec<Value>)> = Vec::new();
    for monitor in monitors {
        match groups.iter_mut().find(|(first, _)| {
            same_component(&first.component, &monitor.component)
                && same_variable(&first.variable, &monitor.variable)
        }) {
            Some((_, entries)) => entries.push(monitor.report_json()),
            None => groups.push((monitor, vec![monitor.report_json()])),
        }
    }
    groups
        .into_iter()
        .map(|(first, entries)| {
            json!({"component": first.component.to_json(), "variable": first.variable.to_json(),
                "variableMonitoring": entries})
        })
        .collect()
}
