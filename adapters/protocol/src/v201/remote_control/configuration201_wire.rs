#[cfg(test)]
mod tests;
use super::{
    configuration201_values::{LocalConfigurationValues201, ProtectedEntry},
    device_model_values::LearnedLimits,
};
use crate::command_registry::configuration201::Request;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
};
use uob_application::CommandClock;
use uob_contracts::{
    CONFIGURATION_BYTES_LIMIT_201, DeviceAttributeType201, DeviceComponent201, DeviceVariable201,
    ResourceRef, SetVariablesReference201, UtcTimestamp,
};

pub(crate) struct DeferredConfigurationCall201 {
    pub provider: Arc<LocalConfigurationValues201>,
    pub clock: Arc<dyn CommandClock>,
    pub resource: ResourceRef,
    pub request: Arc<Request>,
    pub limits: Arc<Mutex<LearnedLimits>>,
    pub active: Arc<Mutex<bool>>,
    pub expires_at: UtcTimestamp,
    pub _metadata: Arc<uob_application::RuntimeReservation>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VariableWire<'a> {
    component: &'a DeviceComponent201,
    variable: &'a DeviceVariable201,
    #[serde(skip_serializing_if = "Option::is_none")]
    attribute_type: Option<DeviceAttributeType201>,
    attribute_value: &'a str,
}
struct VariablesWire<'a> {
    request: &'a SetVariablesReference201,
    entries: &'a BTreeMap<String, ProtectedEntry>,
}
impl Serialize for VariablesWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.request.set_variable_data.len()))?;
        for entry in &self.request.set_variable_data {
            let Some(ProtectedEntry::Variable { value, .. }) =
                self.entries.get(&entry.value_reference)
            else {
                return Err(serde::ser::Error::custom("protected reference unavailable"));
            };
            seq.serialize_element(&VariableWire {
                component: &entry.component,
                variable: &entry.variable,
                attribute_type: entry.attribute_type,
                attribute_value: value.as_str(),
            })?;
        }
        seq.end()
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VariablePayload<'a> {
    set_variable_data: VariablesWire<'a>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkPayload<'a> {
    configuration_slot: i32,
    connection_data: &'a serde_json::value::RawValue,
}
#[derive(Default)]
struct ByteCounter(usize);
impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl DeferredConfigurationCall201 {
    fn write<W: std::io::Write>(
        &self,
        entries: &BTreeMap<String, ProtectedEntry>,
        message_id: &str,
        writer: W,
    ) -> Option<()> {
        match self.request.as_ref() {
            Request::Variables(request) => serde_json::to_writer(
                writer,
                &(
                    2,
                    message_id,
                    "SetVariables",
                    VariablePayload {
                        set_variable_data: VariablesWire { request, entries },
                    },
                ),
            )
            .ok(),
            Request::Network(request) => {
                let ProtectedEntry::Network(value) = entries.get(&request.profile_reference)?
                else {
                    return None;
                };
                serde_json::to_writer(
                    writer,
                    &(
                        2,
                        message_id,
                        "SetNetworkProfile",
                        NetworkPayload {
                            configuration_slot: request.configuration_slot,
                            connection_data: value.profile.raw(),
                        },
                    ),
                )
                .ok()
            }
        }
    }
    fn resolve<T>(
        &self,
        message_id: &str,
        use_frame: impl FnOnce(&BTreeMap<String, ProtectedEntry>, usize) -> Option<T>,
    ) -> Option<T> {
        let active = self.active.lock().ok()?;
        if !*active {
            return None;
        }
        let limits = self.limits.lock().ok()?;
        let now = self.clock.now();
        if now >= self.expires_at {
            return None;
        }
        self.provider
            .with_entries(&self.resource, self.request.as_ref(), now, |entries| {
                let mut count = ByteCounter::default();
                self.write(entries, message_id, &mut count)?;
                let allowed = match self.request.as_ref() {
                    Request::Variables(request) => {
                        limits.allows_variables(request.set_variable_data.len(), count.0)
                    }
                    Request::Network(_) => count.0 <= CONFIGURATION_BYTES_LIMIT_201,
                };
                if !allowed {
                    return None;
                }
                use_frame(entries, count.0)
            })?
    }
    pub(crate) fn wire_size(&self, message_id: &str) -> Option<usize> {
        self.resolve(message_id, |_, size| Some(size))
    }
    /// Resolve all entries while holding policy locks through the first socket-send poll.
    /// Revocation, generation teardown and limit tightening linearize before this poll or
    /// after transmission has begun. Locks are never held over an asynchronous suspension.
    pub(crate) async fn send_at_boundary(
        &self,
        connection: &mut crate::StationConnection,
        message_id: &str,
    ) -> Option<Result<(), axum::Error>> {
        self.send_with(message_id, |message| connection.send(message))
            .await
    }

    async fn send_with<F>(
        &self,
        message_id: &str,
        factory: impl FnOnce(axum::extract::ws::Message) -> F,
    ) -> Option<Result<(), axum::Error>>
    where
        F: Future<Output = Result<(), axum::Error>>,
    {
        let mut factory = Some(factory);
        let mut send = std::pin::pin!(None::<F>);
        std::future::poll_fn(|cx| {
            if let Some(future) = send.as_mut().as_pin_mut() {
                return future.poll(cx).map(Some);
            }
            let started = self.resolve(message_id, |entries, size| {
                let mut frame = Vec::with_capacity(size);
                if self.write(entries, message_id, &mut frame).is_none() {
                    super::configuration201_values::wipe(&mut frame);
                    return None;
                }
                let frame = String::from_utf8(frame).expect("JSON UTF-8");
                send.as_mut().set(Some(factory.take().expect("single send")(
                    axum::extract::ws::Message::Text(frame.into()),
                )));
                Some(
                    send.as_mut()
                        .as_pin_mut()
                        .expect("initialized send")
                        .poll(cx),
                )
            });
            match started {
                Some(poll) => poll.map(Some),
                None => std::task::Poll::Ready(None),
            }
        })
        .await
    }
}
