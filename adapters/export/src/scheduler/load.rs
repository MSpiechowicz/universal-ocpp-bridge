use std::io::{self, Write};

use uob_application::{
    CommittedRecordField, EXPORT_RECORD_CHUNK_BYTES, ExportDeliveryClaim, ExportPendingDescriptor,
    ExportSpool, ExportSpoolNamespace, RuntimeReservation, RuntimeResourceBudget, WorkClass,
};
use uob_contracts::{ExportBatch, ExportRecord};

use super::ExportSchedulerError;

pub(super) async fn load_batch(
    spool: &dyn ExportSpool,
    namespace: &ExportSpoolNamespace,
    budget: &RuntimeResourceBudget,
    claim: &ExportDeliveryClaim,
    max_records: usize,
    max_bytes: usize,
) -> Result<(ExportBatch, RuntimeReservation, usize), ExportSchedulerError> {
    if claim.items.is_empty() || claim.items.len() > max_records {
        return Err(ExportSchedulerError::InvalidPending(
            "claim.provider_record_limit",
        ));
    }
    // Match the spool's conservative claim estimate: even escaped JSON and the
    // provider envelope remain admitted without reserving a full 256 KiB for
    // a handful of records. Source chunks have their own short-lived guards.
    let estimated_bytes = claim
        .items
        .iter()
        .try_fold(256_usize, |bytes, item| {
            item.record_id_len
                .checked_add(item.committed_at_len)
                .and_then(|length| length.checked_add(item.payload_len))
                .and_then(|length| usize::try_from(length).ok())
                .and_then(|length| length.checked_mul(6))
                .and_then(|length| length.checked_add(4096))
                .and_then(|length| bytes.checked_add(length))
        })
        .ok_or(ExportSchedulerError::InvalidPending(
            "claim.length_overflow",
        ))?;
    if estimated_bytes > max_bytes {
        return Err(ExportSchedulerError::InvalidPending(
            "claim.provider_byte_limit",
        ));
    }
    let reservation = budget
        .try_reserve(WorkClass::ExporterBatch, estimated_bytes)
        .map_err(|_| ExportSchedulerError::MemoryPressure)?;
    let mut records = Vec::with_capacity(claim.items.len());
    for descriptor in &claim.items {
        let raw_length = descriptor
            .record_id_len
            .checked_add(descriptor.committed_at_len)
            .and_then(|sum| sum.checked_add(descriptor.payload_len))
            .ok_or(ExportSchedulerError::InvalidPending(
                "record.length_overflow",
            ))?;
        if raw_length > max_bytes as u64 {
            return Err(ExportSchedulerError::InvalidPending("record.oversized"));
        }
        let id = read_field(
            spool,
            namespace,
            descriptor,
            CommittedRecordField::RecordId,
            descriptor.record_id_len,
        )
        .await?;
        let committed_at = read_field(
            spool,
            namespace,
            descriptor,
            CommittedRecordField::CommittedAt,
            descriptor.committed_at_len,
        )
        .await?;
        let payload = read_field(
            spool,
            namespace,
            descriptor,
            CommittedRecordField::Payload,
            descriptor.payload_len,
        )
        .await?;
        let record: ExportRecord = serde_json::from_slice(&payload)
            .map_err(|_| ExportSchedulerError::InvalidPending("record.invalid_payload"))?;
        if id != record.metadata().identity.record_id.as_str().as_bytes() {
            return Err(ExportSchedulerError::InvalidPending(
                "record.identity_mismatch",
            ));
        }
        let _: uob_contracts::UtcTimestamp = serde_json::from_slice(&committed_at)
            .map_err(|_| ExportSchedulerError::InvalidPending("record.invalid_timestamp"))?;
        records.push(record);
    }
    let batch = ExportBatch::new(
        claim.batch_id.clone(),
        namespace.destination.clone(),
        records,
    )
    .map_err(|_| ExportSchedulerError::InvalidPending("batch.invalid_identity"))?;
    // Count the exact canonical JSON representation without materializing a second batch.
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, &batch)
        .map_err(|_| ExportSchedulerError::InvalidPending("batch.encoding"))?;
    if counter.bytes > max_bytes {
        return Err(ExportSchedulerError::InvalidPending("batch.oversized"));
    }
    Ok((batch, reservation, counter.bytes))
}

async fn read_field(
    spool: &dyn ExportSpool,
    namespace: &ExportSpoolNamespace,
    descriptor: &ExportPendingDescriptor,
    field: CommittedRecordField,
    length: u64,
) -> Result<Vec<u8>, ExportSchedulerError> {
    let capacity = usize::try_from(length)
        .map_err(|_| ExportSchedulerError::InvalidPending("record.length_overflow"))?;
    let mut bytes = Vec::with_capacity(capacity);
    while bytes.len() < capacity {
        let offset = bytes.len() as u64;
        let count = EXPORT_RECORD_CHUNK_BYTES.min(capacity - bytes.len());
        let chunk = spool
            .pending_chunk(namespace.clone(), descriptor.clone(), field, offset, count)
            .await
            .map_err(ExportSchedulerError::Spool)?;
        if chunk.field != field
            || chunk.offset != offset
            || !chunk.is_valid_for(length)
            || chunk.bytes.is_empty()
            || chunk.next_offset <= offset
        {
            return Err(ExportSchedulerError::InvalidPending("record.invalid_chunk"));
        }
        bytes.extend_from_slice(&chunk.bytes);
    }
    Ok(bytes)
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
