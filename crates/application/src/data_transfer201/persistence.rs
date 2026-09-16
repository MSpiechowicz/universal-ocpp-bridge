//! Durable, sanitized OCPP 2.0.1 `DataTransfer` receipt and outcome facts.

use uob_contracts::{StationSnapshot, TypedValue, UtcTimestamp};

use crate::{AtomicStoreWrite, OperationalStore, registration};

use super::{
    Error, LAST_STATUS, OUTBOUND_STATUS, Observation, OutboundStatus, RECEIVED_COUNT,
    REQUEST_BYTES, RESPONSE_BYTES, Reply, StatusInfo,
};

/// Atomically records a previously routed OCPP 2.0.1 `DataTransfer` reply.
///
/// Vendor identifiers, opaque data, `customData`, and `statusInfo` are never persisted. The
/// supplied snapshot changes only after the complete storage write commits successfully.
///
/// # Errors
///
/// Returns [`Error::NotRegistered`] for any non-current accepted OCPP 2.0.1 registration.
pub async fn commit_reply<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    request: &Observation,
    reply: Reply,
    now: UtcTimestamp,
) -> Result<Reply, Error> {
    registration::v201::accepted(snapshot).map_err(|_| Error::NotRegistered)?;
    request.validate()?;
    reply.validate()?;

    let mut next = snapshot.clone();
    registration::set(
        &mut next.current_values,
        LAST_STATUS,
        Some(TypedValue::Text(reply.status.as_str().to_owned())),
        None,
        now,
    );
    registration::set(
        &mut next.current_values,
        RECEIVED_COUNT,
        Some(TypedValue::UnsignedInteger(received_count(snapshot))),
        None,
        now,
    );
    registration::set(
        &mut next.current_values,
        REQUEST_BYTES,
        Some(TypedValue::UnsignedInteger(request_bytes(request))),
        None,
        now,
    );
    registration::set(
        &mut next.current_values,
        RESPONSE_BYTES,
        Some(TypedValue::UnsignedInteger(response_bytes(&reply))),
        None,
        now,
    );
    registration::activity(&mut next, now);

    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    store.write_atomic(write).await.map_err(Error::Storage)?;
    *snapshot = next;
    Ok(reply)
}

/// Atomically records one sanitized outbound OCPP 2.0.1 `DataTransfer` outcome.
///
/// It records uncertainty before enqueueing and a final lifecycle result afterwards, but never
/// stores vendor identifiers, request data, response data, extensions, or status details.
///
/// # Errors
///
/// Rejects a non-current accepted OCPP 2.0.1 registration or a failed storage commit.
pub async fn commit_outbound<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    status: OutboundStatus,
    now: UtcTimestamp,
) -> Result<(), Error> {
    registration::v201::accepted(snapshot).map_err(|_| Error::NotRegistered)?;

    let mut next = snapshot.clone();
    registration::set(
        &mut next.current_values,
        OUTBOUND_STATUS,
        Some(TypedValue::Text(status.as_str().to_owned())),
        None,
        now,
    );
    registration::activity(&mut next, now);

    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    store.write_atomic(write).await.map_err(Error::Storage)?;
    *snapshot = next;
    Ok(())
}

fn received_count(snapshot: &StationSnapshot) -> u64 {
    snapshot
        .current_values
        .iter()
        .find(|value| value.point_id.as_str() == RECEIVED_COUNT)
        .and_then(|value| match value.value {
            Some(TypedValue::UnsignedInteger(count)) => Some(count),
            _ => None,
        })
        .unwrap_or(0)
        .saturating_add(1)
}

fn request_bytes(request: &Observation) -> u64 {
    (request.vendor_id.len()
        + request.message_id.as_ref().map_or(0, String::len)
        + request.data.as_ref().map_or(0, |value| value.encoded_bytes)
        + request
            .custom_data
            .as_ref()
            .map_or(0, |value| value.encoded_bytes)) as u64
}

fn response_bytes(reply: &Reply) -> u64 {
    (reply.status.as_str().len()
        + reply.data.as_ref().map_or(0, |value| value.encoded_bytes)
        + reply
            .custom_data
            .as_ref()
            .map_or(0, |value| value.encoded_bytes)
        + reply.status_info.as_ref().map_or(0, status_info_bytes)) as u64
}

fn status_info_bytes(value: &StatusInfo) -> usize {
    value.reason_code.len()
        + value.additional_info.as_ref().map_or(0, String::len)
        + value
            .custom_data
            .as_ref()
            .map_or(0, |data| data.encoded_bytes)
}
