use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;

use ocpp_client::ocpp_types::v16::common::BootNotificationResponseStatus as BootStatus;
use ocpp_client::ocpp_types::v16::{
    BootNotificationResponse, TriggerMessageRequest as TriggerRequest16,
};
use ocpp_client::ocpp_types::v201::{
    BootNotificationResponse as BootResponse201, TriggerMessageRequest as TriggerRequest201,
};
use ocpp_client::{TransportError, TransportEvent, TransportStream};
use serde_json::Value;
use tokio::sync::watch;

use super::TriggerBarrier;

pub(super) struct TriggerStream {
    pub(super) inner: Box<dyn TransportStream>,
    pub(super) barrier: TriggerBarrier,
    pub(super) armed: watch::Receiver<bool>,
}

impl TransportStream for TriggerStream {
    fn recv<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<TransportEvent>, TransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            // The read loop starts before typed handlers are registered; hold
            // inbound frames until each captured ID can reach its callback.
            while !*self.armed.borrow_and_update() {
                self.armed
                    .changed()
                    .await
                    .expect("trigger handler registration sender");
            }
            let event = self.inner.recv().await?;
            if let Some(TransportEvent::Frame(text)) = &event {
                if let Ok(Value::Array(frame)) = serde_json::from_str::<Value>(text) {
                    self.record_trigger(&frame)?;
                    self.record_boot(&frame);
                }
            } else if event.is_none() {
                self.barrier.generation.fetch_add(1, Ordering::SeqCst);
                self.barrier.clear();
            }
            Ok(event)
        })
    }
}

impl TriggerStream {
    fn record_trigger(&self, frame: &[Value]) -> Result<(), TransportError> {
        // The client's RawCall parser drops envelopes with any extra element.
        // Never reserve a FIFO slot for a CALL the typed handler cannot see.
        if frame.len() != 4
            || frame.first().and_then(Value::as_u64) != Some(2)
            || frame.get(2).and_then(Value::as_str) != Some("TriggerMessage")
        {
            return Ok(());
        }
        let Some(payload) = frame.get(3) else {
            return Ok(());
        };
        let valid = match self.barrier.version {
            ocpp_client::OcppVersion::V1_6 => {
                serde_json::from_value::<TriggerRequest16>(payload.clone()).is_ok()
            }
            ocpp_client::OcppVersion::V2_0_1 => serde_json::from_value::<
                TriggerRequest201<ocpp_client::ocpp_types::v201::common::CustomData>,
            >(payload.clone())
            .is_ok(),
        };
        if !valid {
            return Ok(());
        }
        let Some(id) = frame.get(1).and_then(Value::as_str) else {
            return Ok(());
        };
        let mut pending = self.barrier.pending.lock().expect("trigger barrier lock");
        if pending.received.len() + pending.ready.len() >= self.barrier.capacity {
            return Err("too many pending trigger requests".into());
        }
        pending.received.push_back((
            id.to_owned(),
            payload
                .as_object()
                .is_some_and(|fields| self.has_unknown_fields(fields)),
        ));
        Ok(())
    }

    fn has_unknown_fields(&self, fields: &serde_json::Map<String, Value>) -> bool {
        fields.keys().any(|key| match self.barrier.version {
            ocpp_client::OcppVersion::V1_6 => key != "requestedMessage" && key != "connectorId",
            ocpp_client::OcppVersion::V2_0_1 => {
                key != "requestedMessage" && key != "evse" && key != "customData"
            }
        }) || (self.barrier.state_201.is_some()
            && fields
                .get("evse")
                .and_then(Value::as_object)
                .is_some_and(|evse| {
                    evse.keys()
                        .any(|key| key != "id" && key != "connectorId" && key != "customData")
                }))
    }

    fn record_boot(&self, frame: &[Value]) {
        let Some(id) = frame.get(1).and_then(Value::as_str) else {
            return;
        };
        if !self
            .barrier
            .pending
            .lock()
            .expect("trigger barrier lock")
            .boot_calls
            .contains_key(id)
        {
            return;
        }
        let Some(accepted) = self.boot_reply(frame) else {
            return;
        };
        if self
            .barrier
            .pending
            .lock()
            .expect("trigger barrier lock")
            .boot_calls
            .remove(id)
            .is_none_or(|sent| sent.elapsed() >= self.barrier.timeout)
        {
            return;
        }
        if let Some(state) = &self.barrier.state_201 {
            state.lock().expect("OCPP 2.0.1 state lock").registered = accepted;
        } else {
            self.barrier
                .state
                .as_ref()
                .expect("OCPP 1.6 barrier")
                .lock()
                .expect("OCPP 1.6 state lock")
                .registered = accepted;
        }
    }

    fn boot_reply(&self, frame: &[Value]) -> Option<bool> {
        match frame.first().and_then(Value::as_u64) {
            Some(3) if frame.len() == 3 => {
                let payload = frame.get(2)?.clone();
                if self.barrier.state_201.is_some() {
                    serde_json::from_value::<
                        BootResponse201<ocpp_client::ocpp_types::v201::common::CustomData>,
                    >(payload)
                    .ok()
                    .map(|response| {
                        response.status
                            == ocpp_client::ocpp_types::v201::common::RegistrationStatusEnum::Accepted
                    })
                } else {
                    serde_json::from_value::<BootNotificationResponse>(payload)
                        .ok()
                        .map(|response| response.status == BootStatus::Accepted)
                }
            }
            Some(4)
                if frame.len() == 5
                    && frame.get(2).and_then(Value::as_str).is_some()
                    && frame.get(3).and_then(Value::as_str).is_some() =>
            {
                Some(false)
            }
            _ => None,
        }
    }
}
