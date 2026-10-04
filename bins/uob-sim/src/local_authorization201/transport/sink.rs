use super::super::{IdToken, native, validation};
use super::{EXCHANGE_GENERATION, ORIGINAL_EVENT, OwnedSink, SharedSink, disconnect};
use ocpp_client::{TransportError, TransportSink};
use serde::Deserialize;
use serde_json::{Value, json};
use std::pin::Pin;
use std::time::Instant;
use zeroize::Zeroize;

pub(super) struct Sink(pub(super) SharedSink);
impl TransportSink for Sink {
    fn send<'a>(
        &'a mut self,
        mut frame: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let mut sink = self.0.lock().await;
            let mut parsed =
                native::PrivateJson(serde_json::from_str::<Value>(&frame).unwrap_or(Value::Null));
            let timeout = sink.timeout;
            ORIGINAL_EVENT
                .try_with(|original| {
                    if let Some(original) = original
                        && parsed.0.get(0) == Some(&json!(2))
                        && parsed.0.get(2).and_then(Value::as_str) == Some("TransactionEvent")
                    {
                        native::wipe(&mut parsed.0[3]);
                        parsed.0[3] = original.0.clone();
                        let exact = serde_json::to_string(&parsed.0)
                            .map_err(|_| "native event encoding failed")?;
                        frame.zeroize();
                        frame = exact;
                    }
                    Ok::<(), TransportError>(())
                })
                .unwrap_or(Ok(()))?;
            let action = parsed.0.get(2).and_then(Value::as_str);
            let boot = action == Some("BootNotification");
            let id = parsed.0.get(1).and_then(Value::as_str).map(str::to_owned);
            let boot_id = if boot { id.clone() } else { None };
            if let Some(id) = &boot_id {
                sink.boots.retain(|_, sent| sent.elapsed() < timeout);
                if sink.boots.len() >= 128 {
                    return Err("native boot capacity".into());
                }
                sink.boots.insert(id.clone(), Instant::now());
                let mut state = sink.state.lock().expect("native state lock");
                if state.socket_generation == sink.generation {
                    state.registered = false;
                }
            }
            if matches!(action, Some("Authorize" | "TransactionEvent"))
                && let (Some(id), Some(token)) = (
                    &id,
                    parsed.0.get(3).and_then(|payload| payload.get("idToken")),
                )
                && validation::valid_token(token)
            {
                sink.authorizations
                    .retain(|_, (_, sent)| sent.elapsed() < timeout);
                if sink.authorizations.len() >= 128 {
                    return Err("native authorization capacity".into());
                }
                let token = IdToken::deserialize(token)
                    .map_err(|_| "invalid native authorization identity")?;
                sink.authorizations
                    .insert(id.clone(), (token, Instant::now()));
            }
            drop(parsed);
            let expected = EXCHANGE_GENERATION.try_with(|generation| *generation).ok();
            let OwnedSink {
                inner,
                state,
                generation,
                ..
            } = &mut *sink;
            let mut send = inner.send(frame);
            let result = std::future::poll_fn(|context| {
                if let Some(expected) = expected {
                    let current = state.lock().expect("native state lock");
                    if expected != *generation
                        || expected != current.socket_generation
                        || !current.socket_connected
                        || (!boot && !current.registered)
                    {
                        return std::task::Poll::Ready(Err(
                            "native exchange generation unavailable".into(),
                        ));
                    }
                }
                send.as_mut().poll(context)
            })
            .await;
            drop(send);
            if result.is_err()
                && let Some(id) = id
            {
                sink.boots.remove(&id);
                sink.authorizations.remove(&id);
            }
            result
        })
    }
    fn ping<'a>(
        &'a mut self,
        value: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move { self.0.lock().await.inner.ping(value).await })
    }
    fn pong<'a>(
        &'a mut self,
        value: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move { self.0.lock().await.inner.pong(value).await })
    }
    fn close<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let mut sink = self.0.lock().await;
            disconnect(&sink.state, sink.generation);
            sink.inner.close().await
        })
    }
}
