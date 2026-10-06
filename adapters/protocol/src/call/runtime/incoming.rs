use super::{
    CallSessionDiagnostic, CorrelationId, FlowEvidence, FlowStage, IncomingCall,
    IncomingCallResponder, Message, OcppCallError, OcppErrorCode, ProtocolEdition, QueuedReply,
    RuntimeResourceBudget, SessionState, StationConnection, WorkClass, emit, frame, mpsc,
    retain_recent, send_capacity_error, send_error, v16, v201,
};

pub(super) async fn report(
    kind: crate::call::reports::ReportKind,
    message_id: String,
    payload: serde_json::Value,
    bytes: usize,
    state: &mut SessionState,
    connection: &mut StationConnection,
    budget: &RuntimeResourceBudget,
) {
    if state.incoming_ids.contains(&message_id) || state.recent_incoming.contains(&message_id) {
        send_error(
            connection,
            &message_id,
            OcppCallError {
                protocol: state.protocol,
                code: OcppErrorCode::OccurrenceConstraintViolation,
                description: "duplicate OCPP CALL message ID",
                field_path: Some("/1"),
            },
        )
        .await;
        return;
    }
    let valid = if state.protocol == ProtocolEdition::Ocpp201 {
        crate::call::reports::route(&state.reports, kind, &payload, bytes, budget)
    } else {
        None
    };
    let Some(valid) = valid else {
        send_error(
            connection,
            &message_id,
            OcppCallError {
                protocol: state.protocol,
                code: OcppErrorCode::NotImplemented,
                description: "native report handling is not enabled",
                field_path: None,
            },
        )
        .await;
        return;
    };
    retain_recent(
        &mut state.recent_incoming,
        message_id.clone(),
        state.history_capacity,
    );
    if valid {
        let encoded = frame::result(&message_id, &serde_json::json!({}));
        let _ = connection.send(Message::Text(encoded.into())).await;
    } else {
        send_error(
            connection,
            &message_id,
            frame::protocol_error(state.protocol, "invalid native report fragment", None),
        )
        .await;
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn process(
    message_id: String,
    bytes: Vec<u8>,
    state: &mut SessionState,
    connection: &mut StationConnection,
    budget: &RuntimeResourceBudget,
    incoming: &mpsc::Sender<IncomingCall>,
    replies: &mpsc::Sender<QueuedReply>,
    diagnostics: &mpsc::Sender<CallSessionDiagnostic>,
) {
    let correlation_id =
        CorrelationId::new(format!("ocpp:{}:{message_id}", state.correlation_prefix))
            .expect("correlation ID");
    let trace = state.span(Some(correlation_id.clone()));
    trace.emit(FlowStage::OcppReceive, FlowEvidence::Completed);
    if state.incoming_ids.contains(&message_id) || state.recent_incoming.contains(&message_id) {
        trace.emit(FlowStage::Deduplication, FlowEvidence::Duplicate);
        emit(
            diagnostics,
            CallSessionDiagnostic::DuplicateIncomingCall {
                message_id: message_id.clone(),
            },
        );
        send_error(
            connection,
            &message_id,
            OcppCallError {
                protocol: state.protocol,
                code: OcppErrorCode::OccurrenceConstraintViolation,
                description: "duplicate OCPP CALL message ID",
                field_path: Some("/1"),
            },
        )
        .await;
        return;
    }
    let decoded = match state.protocol {
        ProtocolEdition::Ocpp16j => v16::decode_call(&bytes),
        ProtocolEdition::Ocpp201 => v201::decode_call(&bytes),
    };
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(error) => {
            trace.emit(FlowStage::Validation, FlowEvidence::Rejected);
            send_error(connection, &message_id, error.call_error()).await;
            return;
        }
    };
    let Ok(reservation) = budget.try_reserve(WorkClass::ChargerRequest, bytes.len()) else {
        send_capacity_error(connection, &message_id, state.protocol).await;
        return;
    };
    trace.emit(FlowStage::Validation, FlowEvidence::Completed);
    let call = IncomingCall {
        trace: trace.clone(),
        call: decoded,
        correlation_id,
        responder: IncomingCallResponder {
            trace,
            message_id: message_id.clone(),
            protocol: state.protocol,
            sender: Some(replies.clone()),
            budget: budget.clone(),
        },
        _reservation: reservation,
    };
    match incoming.try_send(call) {
        Ok(()) => {
            state.incoming_ids.insert(message_id.clone());
            retain_recent(
                &mut state.recent_incoming,
                message_id,
                state.history_capacity,
            );
        }
        Err(error) => {
            let mut rejected = error.into_inner();
            rejected.responder.sender = None;
            send_capacity_error(connection, &message_id, state.protocol).await;
        }
    }
}
