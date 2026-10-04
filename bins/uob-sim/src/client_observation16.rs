use std::sync::{Arc, Mutex};

use ocpp_client::ocpp_types::v16::StatusNotificationRequest;
use serde::Deserialize;

use crate::{
    Ocpp16State, SimulatorAction, SimulatorCall, SimulatorClientError, TraceBuffer, TraceKind,
};

pub(crate) fn observe_local_authorization(
    state: &Arc<Mutex<Ocpp16State>>,
    call: &SimulatorCall,
    response: &serde_json::Value,
    traces: &TraceBuffer,
) -> Result<(), SimulatorClientError> {
    if !matches!(
        call.action,
        SimulatorAction::Authorize
            | SimulatorAction::StartTransaction
            | SimulatorAction::StopTransaction
    ) {
        return Ok(());
    }
    let Some(token) = call
        .payload
        .get("idTag")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(());
    };
    let Some(info) = response.get("idTagInfo") else {
        return Ok(());
    };
    let info = crate::local_authorization::NativeInfo::deserialize(info).map_err(|_| {
        SimulatorClientError::Protocol("invalid native authorization response".to_owned())
    })?;
    let Some(local) = state.lock().expect("OCPP 1.6 state lock").local.clone() else {
        return Err(SimulatorClientError::Protocol(
            "native station stopped".to_owned(),
        ));
    };
    let conflict = match local.observe_central(token, info) {
        Ok(conflict) => conflict,
        Err("invalid_central_authorization") => {
            return Err(SimulatorClientError::Protocol(
                "invalid native authorization response".to_owned(),
            ));
        }
        Err(_) => {
            // The native authorization reply remains known even when the
            // separate bounded durable cache update fails.
            traces.push(TraceKind::Failed, "authorization_cache_update_failed");
            return Ok(());
        }
    };
    if conflict {
        let queued = state
            .lock()
            .expect("OCPP 1.6 state lock")
            .notifications
            .as_ref()
            .and_then(tokio::sync::mpsc::WeakSender::upgrade)
            .is_some_and(|sender| sender.try_send(crate::Command::LocalListConflict).is_ok());
        if !queued {
            traces.push(
                TraceKind::Failed,
                "local_list_conflict_notification_unavailable",
            );
        }
    }
    Ok(())
}

pub(crate) async fn notify_conflict(
    client: &ocpp_client::ocpp_1_6::OCPP1_6Client,
    traces: &TraceBuffer,
) {
    let mut attempt = NotificationAttempt {
        traces,
        completed: false,
    };
    let notification: StatusNotificationRequest = serde_json::from_value(serde_json::json!({
        "connectorId":0, "errorCode":"LocalListConflict", "status":"Available"
    }))
    .expect("native conflict status");
    traces.push(TraceKind::ChargingCallSent, "StatusNotification");
    if client.send_status_notification(notification).await.is_err() {
        traces.push(
            TraceKind::Failed,
            "local_list_conflict_notification_uncertain",
        );
    } else {
        traces.push(TraceKind::ChargingCallResult, "StatusNotification");
    }
    attempt.completed = true;
}

struct NotificationAttempt<'a> {
    traces: &'a TraceBuffer,
    completed: bool,
}

impl Drop for NotificationAttempt<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.traces.push(
                TraceKind::Failed,
                "local_list_conflict_notification_uncertain",
            );
        }
    }
}
