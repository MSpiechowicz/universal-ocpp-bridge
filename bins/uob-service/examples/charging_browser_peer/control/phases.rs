//! Station-originated observations for explicitly requested control phases.
use super::{Edition, Phase, Result, State};
use crate::timestamp;
use serde_json::{Value, json};
pub(super) fn phase_calls(
    state: &mut State,
    edition: Edition,
    phase: Phase,
) -> Result<Vec<(&'static str, Value)>> {
    if matches!(phase, Phase::Counts) {
        return Ok(Vec::new());
    }
    let stamp = timestamp()?;
    match (edition, phase) {
        (Edition::Alpha, Phase::Prepare) => prepare_alpha(state, &stamp),
        (Edition::Bravo, Phase::Prepare) if !state.prepared => Ok(prepare_bravo(state, &stamp)),
        (Edition::Alpha, Phase::Start) => {
            let start = state.start16.take().ok_or("no accepted remote start")?;
            if state.active16.is_some() {
                return Err("transaction already active");
            }
            state.counts.started = state.counts.started.saturating_add(1);
            Ok(vec![(
                "StartTransaction",
                json!({"connectorId":start.connector,"idTag":start.token,"meterStart":0,"timestamp":stamp}),
            )])
        }
        (Edition::Bravo, Phase::Start) => {
            let start = state.start201.take().ok_or("no accepted remote start")?;
            if state.active201.is_some() {
                return Err("transaction already active");
            }
            let id = format!("browser-remote-{}", start.remote_id);
            state.active201 = Some(id.clone());
            state.counts.started = state.counts.started.saturating_add(1);
            Ok(vec![(
                "TransactionEvent",
                json!({"eventType":"Started","timestamp":stamp,"triggerReason":"RemoteStart","seqNo":0,"transactionInfo":{"transactionId":id,"remoteStartId":start.remote_id},"idToken":start.token,"evse":{"id":start.evse,"connectorId":1}}),
            )])
        }
        (Edition::Alpha, Phase::Stop) if state.stop_requested => {
            let id = state.active16.take().ok_or("no active transaction")?;
            state.stop_requested = false;
            state.counts.ended = state.counts.ended.saturating_add(1);
            Ok(vec![(
                "StopTransaction",
                json!({"transactionId":id,"meterStop":0,"timestamp":stamp}),
            )])
        }
        (Edition::Bravo, Phase::Stop) if state.stop_requested => {
            let id = state.active201.take().ok_or("no active transaction")?;
            state.stop_requested = false;
            state.counts.ended = state.counts.ended.saturating_add(1);
            Ok(vec![(
                "TransactionEvent",
                json!({"eventType":"Ended","timestamp":stamp,"triggerReason":"RemoteStop","seqNo":1,"transactionInfo":{"transactionId":id,"stoppedReason":"Remote"},"evse":{"id":1,"connectorId":1}}),
            )])
        }
        (Edition::Alpha, Phase::Availability) => {
            let change = state
                .availability16
                .take()
                .ok_or("no accepted availability request")?;
            let status = if change.operative {
                "Available"
            } else {
                "Unavailable"
            };
            state.counts.availability_observed =
                state.counts.availability_observed.saturating_add(1);
            let connectors: &[u64] = if change.connector == 0 { &[0, 1] } else { &[1] };
            Ok(connectors.iter().map(|connector| ("StatusNotification", json!({"connectorId":connector,"status":status,"errorCode":"NoError","timestamp":stamp}))).collect())
        }
        (Edition::Bravo, Phase::Availability) => {
            let change = state
                .availability201
                .take()
                .ok_or("no accepted availability request")?;
            let status = if change.operative {
                "Available"
            } else {
                "Unavailable"
            };
            state.counts.availability_observed =
                state.counts.availability_observed.saturating_add(1);
            let evses: &[u64] = if change.evse.is_some() {
                &[change.evse.unwrap_or(1)]
            } else {
                &[1, 2]
            };
            Ok(evses.iter().map(|evse| ("StatusNotification", json!({"evseId":evse,"connectorId":1,"connectorStatus":status,"timestamp":stamp}))).collect())
        }
        _ => Err("invalid peer observation phase"),
    }
}

fn prepare_alpha(state: &mut State, stamp: &str) -> Result<Vec<(&'static str, Value)>> {
    let id = state
        .seed_transaction
        .take()
        .ok_or("seed transaction already cleared")?;
    state.prepared = true;
    Ok(vec![
        (
            "StopTransaction",
            json!({"transactionId":id,"meterStop":0,"timestamp":stamp}),
        ),
        (
            "StatusNotification",
            json!({"connectorId":0,"status":"Available","errorCode":"NoError","timestamp":stamp}),
        ),
        (
            "StatusNotification",
            json!({"connectorId":1,"status":"Available","errorCode":"NoError","timestamp":stamp}),
        ),
    ])
}

fn prepare_bravo(state: &mut State, stamp: &str) -> Vec<(&'static str, Value)> {
    state.prepared = true;
    vec![
        (
            "TransactionEvent",
            json!({"eventType":"Ended","timestamp":stamp,"triggerReason":"StopAuthorized","seqNo":1,"transactionInfo":{"transactionId":"browser-bravo-tx-1","stoppedReason":"Remote"},"evse":{"id":1,"connectorId":1}}),
        ),
        (
            "TransactionEvent",
            json!({"eventType":"Ended","timestamp":stamp,"triggerReason":"StopAuthorized","seqNo":1,"transactionInfo":{"transactionId":"browser-bravo-tx-2","stoppedReason":"Remote"},"evse":{"id":2,"connectorId":1}}),
        ),
        (
            "StatusNotification",
            json!({"timestamp":stamp,"connectorStatus":"Available","evseId":1,"connectorId":1}),
        ),
        (
            "StatusNotification",
            json!({"timestamp":stamp,"connectorStatus":"Available","evseId":2,"connectorId":1}),
        ),
    ]
}
