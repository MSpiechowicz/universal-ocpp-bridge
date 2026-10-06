#[cfg(test)]
#[path = "reservation_tests.rs"]
mod tests;
use super::{RemoteControlSession, mapping, reservation_values::ReservationValues201};
use crate::{
    OutboundCall, SessionCallOutcome, SessionSubmitError,
    command_registry::reservation201::{self, Request},
};
use serde_json::Value;
use std::{
    future::Future,
    sync::{Arc, Mutex, RwLock},
};
use uob_application::{
    CommandClock, CommandDispatchOutcome, ReservationMutation201, ReservationMutationKind201,
};
use uob_contracts::{
    CancelReservationStatus201, Command, CommandErrorCode, CommandOperation, CorrelationId,
    ReservationReconciliation201, ReservationResult201, ReservationState201, ReserveNowStatus201,
    StationSnapshot, UtcTimestamp,
};

pub type ReservationGrant201 = dyn Fn(&Command<Value>, UtcTimestamp) -> bool + Send + Sync;
impl RemoteControlSession {
    /// Installs the owner-only provider, explicit `NonEvseSpecific` support and the privileged
    /// grant for this authenticated socket only. Absent grant keeps every reservation closed.
    #[must_use]
    pub fn with_reservations_201(
        mut self,
        provider: Option<Arc<ReservationValues201>>,
        non_evse_specific: bool,
        grant: Arc<ReservationGrant201>,
    ) -> Self {
        self.reservation_values = provider;
        self.reserve_non_evse_specific = non_evse_specific;
        self.reservation_grant = Some(grant);
        self
    }
    pub(super) fn reservation_context(
        &self,
        command: &Command<Value>,
        generation: Option<u64>,
        now: UtcTimestamp,
    ) -> Result<Option<ReservationMutation201>, CommandErrorCode> {
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return Ok(None);
        };
        if !reservation201::ACTIONS.contains(&operation.action.as_str()) {
            return Ok(None);
        }
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| CommandErrorCode::PolicyRejected)?;
        let request = reservation201::validate(&command.resource, operation)?;
        check(
            command,
            &snapshot,
            &request,
            self.reserve_non_evse_specific,
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
                (request.id, ReservationMutationKind201::Reserve(candidate))
            }
            Request::Cancel(id) => (id, ReservationMutationKind201::Cancel),
        };
        Ok(Some(ReservationMutation201 {
            station: snapshot.station.clone(),
            request_id: command.request_id.clone(),
            reservation_id,
            admitted_at: now,
            generation: generation.ok_or(CommandErrorCode::StationDisconnected)?,
            mutation,
        }))
    }
    pub(super) async fn dispatch_reservation(
        &self,
        command: Command<Value>,
    ) -> CommandDispatchOutcome {
        let now = self.clock.now();
        if now >= command.expires_at {
            return mapping::not_sent(CommandErrorCode::Expired);
        }
        let CommandOperation::Ocpp(operation) = &command.operation else {
            return mapping::not_sent(CommandErrorCode::InvalidParameters);
        };
        let request = match reservation201::validate(&command.resource, operation) {
            Ok(request) => request,
            Err(code) => return mapping::not_sent(code),
        };
        let action = match request {
            Request::Reserve(_) => "ReserveNow",
            Request::Cancel(_) => "CancelReservation",
        };
        let pending = {
            let Ok(snapshot) = self.snapshot.read() else {
                return mapping::not_sent(CommandErrorCode::PolicyRejected);
            };
            if !matches!(snapshot.connectivity, uob_contracts::Connectivity::Connected { connected_at, .. } if command.admitted_at >= connected_at)
            {
                return mapping::not_sent(CommandErrorCode::StationDisconnected);
            }
            if let Err(code) = check(
                &command,
                &snapshot,
                &request,
                self.reserve_non_evse_specific,
                self.reservation_grant.as_ref(),
                now,
            ) {
                return mapping::not_sent(code);
            }
            if self.handle.is_closed() {
                return mapping::not_sent(CommandErrorCode::StationDisconnected);
            }
            let deferred = DeferredReservationCall201 {
                command: command.clone(),
                request,
                snapshot: self.snapshot.clone(),
                provider: self.reservation_values.clone(),
                grant: self.reservation_grant.clone(),
                active: self.configuration_active.clone(),
                non_evse_specific: self.reserve_non_evse_specific,
                clock: self.clock.clone(),
            };
            let call = OutboundCall {
                message_id: command.request_id.as_str().to_owned(),
                action: operation.action.clone(),
                payload: operation.payload.clone(),
                correlation_id: command.correlation_id.clone().unwrap_or_else(|| {
                    CorrelationId::new(command.request_id.as_str()).expect("request identity")
                }),
            };
            let remaining = (command.expires_at.into_inner() - now.into_inner()).unsigned_abs();
            let deadline =
                tokio::time::Instant::now() + remaining.min(std::time::Duration::from_hours(24));
            match self
                .handle
                .try_reservation_201_call_before(call, deadline, deferred)
            {
                Ok(pending) => pending,
                Err(error) => {
                    return mapping::not_sent(match error {
                        SessionSubmitError::Closed => CommandErrorCode::StationDisconnected,
                        SessionSubmitError::InvalidRequest => CommandErrorCode::InvalidParameters,
                        SessionSubmitError::Resource(_) | SessionSubmitError::Full => {
                            CommandErrorCode::PolicyRejected
                        }
                    });
                }
            }
        };
        match pending.receive().await {
            SessionCallOutcome::Result { payload, .. } => {
                response(action, &payload, &command, self.clock.now())
            }
            // A CALLERROR does not prove the station left its reservation table unchanged.
            SessionCallOutcome::Error { .. }
            | SessionCallOutcome::TimedOut { .. }
            | SessionCallOutcome::TransmissionUncertain { .. } => mapping::uncertain(),
            SessionCallOutcome::NotTransmitted { .. } => {
                mapping::not_sent(if self.clock.now() >= command.expires_at {
                    CommandErrorCode::Expired
                } else {
                    CommandErrorCode::PolicyRejected
                })
            }
        }
    }
}
fn check(
    command: &Command<Value>,
    snapshot: &StationSnapshot,
    request: &Request,
    non_evse_specific: bool,
    grant: Option<&Arc<ReservationGrant201>>,
    now: UtcTimestamp,
) -> Result<(), CommandErrorCode> {
    if !grant.is_some_and(|g| g(command, now)) {
        return Err(CommandErrorCode::Unauthorized);
    }
    uob_application::registration::v201::accepted(snapshot)
        .map_err(|_| CommandErrorCode::PolicyRejected)?;
    let capabilities = mapping::capabilities(snapshot, &command.resource)
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
        // H01.FR.18/19: unspecified-EVSE reservations need explicit NonEvseSpecific support.
        if request.evse_id.is_none() && !non_evse_specific {
            return Err(CommandErrorCode::UnsupportedOperation);
        }
        if request.expiry_date_time <= now {
            return Err(CommandErrorCode::Expired);
        }
    }
    Ok(())
}

pub(crate) struct DeferredReservationCall201 {
    pub command: Command<Value>,
    pub request: Request,
    pub snapshot: Arc<RwLock<StationSnapshot>>,
    pub provider: Option<Arc<ReservationValues201>>,
    pub grant: Option<Arc<ReservationGrant201>>,
    pub active: Arc<Mutex<bool>>,
    pub non_evse_specific: bool,
    pub clock: Arc<dyn CommandClock>,
}
#[derive(Default)]
struct ByteCounter(usize);
impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl DeferredReservationCall201 {
    /// Rechecks socket generation, grant, registration, capability, expiry and the protected
    /// record immediately before exposing native material to the encoder.
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
            self.non_evse_specific,
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
            let mut count = ByteCounter::default();
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
        let mut count = ByteCounter::default();
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

/// Exact native status of a pinned response; `statusInfo` is validated and then dropped.
pub(crate) fn native_status(action: &str, payload: &Value) -> Option<ReserveNowStatus201> {
    let index = match action {
        "ReserveNow" => 1,
        "CancelReservation" => 3,
        _ => return None,
    };
    if !reservation201::valid_native(index, payload) {
        return None;
    }
    let status: ReserveNowStatus201 = serde::Deserialize::deserialize(&payload["status"]).ok()?;
    (action == "ReserveNow"
        || matches!(
            status,
            ReserveNowStatus201::Accepted | ReserveNowStatus201::Rejected
        ))
    .then_some(status)
}

/// Maps a validated in-time or late native acknowledgement to value-free evidence.
#[must_use]
pub fn response(
    action: &str,
    payload: &Value,
    command: &Command<Value>,
    now: UtcTimestamp,
) -> CommandDispatchOutcome {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return mapping::uncertain();
    };
    let Some(status) = native_status(action, payload) else {
        return mapping::uncertain();
    };
    let reconciliation = ReservationReconciliation201 {
        revision: 0,
        state: ReservationState201::Pending,
        observed_at: now,
        source_time: None,
    };
    let evidence = match (
        action,
        reservation201::validate(&command.resource, operation),
    ) {
        ("ReserveNow", Ok(Request::Reserve(request))) => ReservationResult201::ReserveNow {
            reservation_id: request.id,
            evse_id: request.evse_id,
            status: Some(status),
            reconciliation,
        },
        ("CancelReservation", Ok(Request::Cancel(id))) => ReservationResult201::CancelReservation {
            reservation_id: id,
            status: Some(match status {
                ReserveNowStatus201::Accepted => CancelReservationStatus201::Accepted,
                _ => CancelReservationStatus201::Rejected,
            }),
            reconciliation,
        },
        _ => return mapping::uncertain(),
    };
    CommandDispatchOutcome::ReservationResponse201(evidence)
}
