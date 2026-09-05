use axum::response::sse::Event;
use futures_util::{Stream, stream};
use std::{collections::BTreeSet, pin::Pin, sync::Arc};
use uob_application::{RetainedEventQuery, TargetPortFuture, TargetQueryPort};

use super::{MAX_PAYLOAD_BYTES, payload, request::safe_cursor};
use crate::{error::IntegrationErrorCode as Error, reads::integration_code};

pub(super) type Events = Pin<Box<dyn Stream<Item = Result<Event, Error>> + Send>>;

pub(super) trait Source: Send + Sync {
    fn open(
        &self,
        query: RetainedEventQuery,
        types: BTreeSet<String>,
    ) -> TargetPortFuture<'_, Events>;
}

pub(super) struct HostSource<E>(pub Arc<dyn TargetQueryPort<E>>);

impl<E: serde::Serialize + Send + 'static> Source for HostSource<E> {
    fn open(
        &self,
        query: RetainedEventQuery,
        types: BTreeSet<String>,
    ) -> TargetPortFuture<'_, Events> {
        Box::pin(async move {
            let resource = query.resource.clone();
            let source = self.0.subscribe_retained_events(query).await?;
            Ok(Box::pin(stream::unfold(
                (source, types, resource),
                |(mut source, types, resource)| async move {
                    let item = std::future::poll_fn(|cx| source.as_mut().poll_event(cx)).await?;
                    let event = item
                        .map_err(|e| integration_code(e.code()))
                        .and_then(|item| {
                            if item.event.resource.bridge_id != resource.bridge_id
                                || item.event.resource.station_id != resource.station_id
                                || item.event.resource.resource != resource.resource
                            {
                                return Err(Error::PermissionDenied);
                            }
                            if !safe_cursor(item.cursor.as_str()) {
                                return Err(Error::InvalidRequest);
                            }
                            let event = Event::default().id(item.cursor.as_str());
                            if !types.is_empty() && !types.contains(item.event.event_type.as_str())
                            {
                                return Ok(event);
                            }
                            let json = payload::encode_json(&item.event, MAX_PAYLOAD_BYTES)
                                .map_err(|error| match error {
                                    payload::PayloadEncodingError::TooLarge => {
                                        Error::PayloadTooLarge
                                    }
                                    payload::PayloadEncodingError::Serialization => {
                                        Error::SourceUnavailable
                                    }
                                })?;
                            Ok(event.event("durable").data(json))
                        });
                    Some((event, (source, types, resource)))
                },
            )) as Events)
        })
    }
}
