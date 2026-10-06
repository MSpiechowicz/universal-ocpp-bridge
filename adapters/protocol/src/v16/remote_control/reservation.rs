#[cfg(test)]
#[path = "reservation_tests.rs"]
mod tests;
use super::{RemoteControlSession, reservation_values::ReservationValues16};
use crate::command_registry::reservation16::{self, Request};
use serde_json::Value;
use std::{
    future::Future,
    sync::{Arc, RwLock},
};
use uob_application::{
    CommandClock, CommandDispatchOutcome, ReservationMutation16, ReservationMutationKind16,
};
use uob_contracts::{
    Command, CommandErrorCode, CommandOperation, ReservationReconciliation16, ReservationResult16,
    ReservationState16, StationSnapshot, UtcTimestamp,
};

pub type ReservationGrant16 = dyn Fn(&Command<Value>, UtcTimestamp) -> bool + Send + Sync;
impl RemoteControlSession {
    pub(super) fn reservation_context(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<ReservationMutation16>, CommandErrorCode> {
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Ok(None);
        };
        if !reservation16::ACTIONS.contains(&operation.action.as_str()) {
            return Ok(None);
        }
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        let request = reservation16::validate(&command.resource, operation)?;
        check(
            command,
            &snapshot,
            &request,
            self.reserve_zero,
            self.reservation_grant.as_ref(),
            now,
        )?;
        if self.handle.is_closed() {
            return Err(CommandErrorCode::StationDisconnected);
        }
        let (reservation_id, mutation) = match request {
            Request::Reserve(request) => {
                let candidate = self
                    .reservation_values
                    .as_ref()
                    .and_then(|p| p.candidate(&command.resource, &request, now))
                    .ok_or(CommandErrorCode::PolicyRejected)?;
                (
                    request.reservation_id,
                    ReservationMutationKind16::Reserve(candidate),
                )
            }
            Request::Cancel(id) => (id, ReservationMutationKind16::Cancel),
        };
        Ok(Some(ReservationMutation16 {
            station: snapshot.station.clone(),
            request_id: command.request_id.clone(),
            reservation_id,
            admitted_at: now,
            generation: generation.ok_or(CommandErrorCode::StationDisconnected)?,
            mutation,
        }))
    }
}
fn check(
    command: &Command<Value>,
    snapshot: &StationSnapshot,
    request: &Request,
    zero: bool,
    grant: Option<&Arc<ReservationGrant16>>,
    now: UtcTimestamp,
) -> Result<(), CommandErrorCode> {
    if !grant.is_some_and(|g| g(command, now)) {
        return Err(CommandErrorCode::Unauthorized);
    }
    uob_application::registration::accepted(snapshot)
        .map_err(|_| CommandErrorCode::PolicyRejected)?;
    let capabilities = super::mapping::capabilities(snapshot, &command.resource)
        .ok_or(CommandErrorCode::InvalidParameters)?;
    command
        .validate_for_dispatch(capabilities, now)
        .map_err(|e| match e {
            uob_contracts::CommandValidationError::Expired => CommandErrorCode::Expired,
            uob_contracts::CommandValidationError::UnsupportedOperation(_) => {
                CommandErrorCode::UnsupportedOperation
            }
        })?;
    if let Request::Reserve(request) = request {
        if request.connector_id == 0 && !zero {
            return Err(CommandErrorCode::UnsupportedOperation);
        }
        if request.expiry_date <= now {
            return Err(CommandErrorCode::Expired);
        }
    }
    Ok(())
}

pub(crate) struct DeferredReservationCall16 {
    pub command: Command<Value>,
    pub request: Request,
    pub snapshot: Arc<RwLock<StationSnapshot>>,
    pub provider: Option<Arc<ReservationValues16>>,
    pub grant: Option<Arc<ReservationGrant16>>,
    pub active: Arc<std::sync::Mutex<bool>>,
    pub zero: bool,
    pub clock: Arc<dyn CommandClock>,
}
impl DeferredReservationCall16 {
    fn with_payload<T>(&self, callback: impl FnOnce(&str, &Value) -> Option<T>) -> Option<T> {
        let active = self.active.lock().ok()?;
        if !*active {
            return None;
        }
        let snapshot = self.snapshot.read().ok()?;
        let now = self.clock.now();
        check(
            &self.command,
            &snapshot,
            &self.request,
            self.zero,
            self.grant.as_ref(),
            now,
        )
        .ok()?;
        let CommandOperation::Ocpp(operation) = &self.command.operation else {
            return None;
        };
        match &self.request {
            Request::Reserve(request) => self.provider.as_ref()?.with_native(
                &self.command.resource,
                request,
                now,
                |native| callback("ReserveNow", native),
            ),
            Request::Cancel(_) => callback("CancelReservation", &operation.payload),
        }
    }
    pub(crate) fn wire_size(&self, message_id: &str) -> Option<usize> {
        self.with_payload(|action, payload| {
            let mut count = super::local_authorization::ByteCounter::default();
            serde_json::to_writer(&mut count, &(2, message_id, action, payload)).ok()?;
            Some(count.0)
        })
    }
    pub(crate) fn metadata_size(
        &self,
        payload: &Value,
        message_id: &str,
        correlation_id: &str,
    ) -> Option<usize> {
        let mut count = super::local_authorization::ByteCounter::default();
        serde_json::to_writer(&mut count, &self.command).ok()?;
        serde_json::to_writer(&mut count, payload).ok()?;
        count
            .0
            .checked_mul(20)?
            .checked_add(message_id.len().checked_mul(4)?)?
            .checked_add(correlation_id.len().checked_mul(4)?)?
            .checked_add(4096)
    }
    pub(crate) async fn send_at_boundary(
        &self,
        connection: &mut crate::StationConnection,
        message_id: &str,
    ) -> Option<Result<(), axum::Error>> {
        self.send_with(message_id, |message| connection.send(message))
            .await
    }
    async fn send_with<F: Future<Output = Result<(), axum::Error>>>(
        &self,
        message_id: &str,
        factory: impl FnOnce(axum::extract::ws::Message) -> F,
    ) -> Option<Result<(), axum::Error>> {
        let mut factory = Some(factory);
        let mut send = std::pin::pin!(None::<F>);
        std::future::poll_fn(|cx| {
            if let Some(future) = send.as_mut().as_pin_mut() {
                return future.poll(cx).map(Some);
            }
            let started = self.with_payload(|action, payload| {
                let frame = serde_json::to_string(&(2, message_id, action, payload)).ok()?;
                send.as_mut().set(Some(factory.take().expect("single send")(
                    axum::extract::ws::Message::Text(frame.into()),
                )));
                Some(
                    send.as_mut()
                        .as_pin_mut()
                        .expect("single protected send")
                        .poll(cx),
                )
            });
            started.map_or(std::task::Poll::Ready(None), |poll| poll.map(Some))
        })
        .await
    }
}

#[must_use]
pub fn response(
    action: &str,
    payload: &Value,
    command: &Command<Value>,
    now: UtcTimestamp,
) -> CommandDispatchOutcome {
    let uncertain = || super::mapping::uncertain();
    if !payload
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.contains_key("status"))
    {
        return uncertain();
    }
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return uncertain();
    };
    let reconciliation = ReservationReconciliation16 {
        revision: 0,
        state: ReservationState16::Pending,
        observed_at: now,
        source_time: None,
    };
    let evidence = match (
        action,
        reservation16::validate(&command.resource, operation),
    ) {
        ("ReserveNow", Ok(Request::Reserve(request))) => {
            let Ok(native) = <rust_ocpp::v1_6::messages::reserve_now::ReserveNowResponse as serde::Deserialize>::deserialize(payload) else { return uncertain(); };
            let status = match native.status {
                rust_ocpp::v1_6::types::ReservationStatus::Accepted => {
                    uob_contracts::ReserveNowStatus16::Accepted
                }
                rust_ocpp::v1_6::types::ReservationStatus::Faulted => {
                    uob_contracts::ReserveNowStatus16::Faulted
                }
                rust_ocpp::v1_6::types::ReservationStatus::Occupied => {
                    uob_contracts::ReserveNowStatus16::Occupied
                }
                rust_ocpp::v1_6::types::ReservationStatus::Rejected => {
                    uob_contracts::ReserveNowStatus16::Rejected
                }
                rust_ocpp::v1_6::types::ReservationStatus::Unavailable => {
                    uob_contracts::ReserveNowStatus16::Unavailable
                }
            };
            ReservationResult16::ReserveNow {
                reservation_id: request.reservation_id,
                connector_id: request.connector_id,
                status: Some(status),
                reconciliation,
            }
        }
        ("CancelReservation", Ok(Request::Cancel(id))) => {
            let Ok(native) = <rust_ocpp::v1_6::messages::cancel_reservation::CancelReservationResponse as serde::Deserialize>::deserialize(payload) else { return uncertain(); };
            let status = match native.status {
                rust_ocpp::v1_6::types::CancelReservationStatus::Accepted => {
                    uob_contracts::CancelReservationStatus16::Accepted
                }
                rust_ocpp::v1_6::types::CancelReservationStatus::Rejected => {
                    uob_contracts::CancelReservationStatus16::Rejected
                }
            };
            ReservationResult16::CancelReservation {
                reservation_id: id,
                status: Some(status),
                reconciliation,
            }
        }
        _ => return uncertain(),
    };
    CommandDispatchOutcome::ReservationResponse16(evidence)
}
