use uob_contracts::ExportRecord;

use crate::{
    CommittedRecordChunkQuery, CommittedRecordChunkResult, CommittedRecordCursor,
    CommittedRecordDescriptor, CommittedRecordField, CommittedRecordPage, CommittedRecordQuery,
    Durability, EXPORT_RECORD_CHUNK_BYTES, OperationalStore, PageLimit, RuntimeResourceBudget,
    StorageError,
};

mod progress;

use progress::{
    leading_gap_commit, prepare_progress, record_begin, status_high_water, stream_checkpoint,
};

use super::spool::{
    ExportSpool, ExportSpoolError, ExportSpoolErrorCode, ExportSpoolGapCommit,
    ExportSpoolNamespace, ExportSpoolRecordAdmission, ExportSpoolStatus, ExportSpoolTransfer,
};

#[derive(Debug)]
pub enum ExportIngestionError {
    Source(StorageError),
    Spool(ExportSpoolError),
    InvalidSourceCursor,
}

impl From<StorageError> for ExportIngestionError {
    fn from(value: StorageError) -> Self {
        Self::Source(value)
    }
}

impl From<ExportSpoolError> for ExportIngestionError {
    fn from(value: ExportSpoolError) -> Self {
        Self::Spool(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportIngestionOutcome {
    pub status: ExportSpoolStatus,
    pub processed: u16,
    pub backpressured: bool,
}

/// Explicit offline coordinator; never invoked from a charger request or a provider task.
pub struct ExportIngestor;
impl ExportIngestor {
    /// Runs up to 100 discovery/record steps per stream, moving one bounded source chunk
    /// into the provisional spool transaction at a time.
    ///
    /// # Errors
    ///
    /// Returns an error if source integrity, namespace, or durable spool operations fail.
    pub async fn ingest_once<C, E, D, S, P>(
        source: &S,
        spool: &P,
        destination: uob_contracts::ExportDestination,
        provider_kind: &str,
        budget: &RuntimeResourceBudget,
    ) -> Result<ExportIngestionOutcome, ExportIngestionError>
    where
        S: OperationalStore<C, E, D, ExportRecord>,
        P: ExportSpool,
    {
        let limit = PageLimit::new(1).map_err(|_| ExportIngestionError::InvalidSourceCursor)?;
        let mut total = 0_u16;
        let mut backpressured = false;
        let mut last_status = None;

        for durability in [Durability::Critical, Durability::BestEffortTelemetry] {
            let progress = ingest_stream::<C, E, D, S, P>(
                source,
                spool,
                &destination,
                provider_kind,
                budget,
                durability,
                limit,
            )
            .await?;
            total += progress.processed;
            backpressured |= progress.backpressured;
            if let Some(status) = progress.last_status {
                last_status = Some(status);
            }
        }

        let status = last_status.ok_or(ExportIngestionError::Spool(ExportSpoolError::new(
            ExportSpoolErrorCode::Busy,
            "optional ingestion memory admission unavailable",
        )))?;
        Ok(ExportIngestionOutcome {
            status,
            processed: total,
            backpressured,
        })
    }
}

struct StreamProgress {
    last_status: Option<ExportSpoolStatus>,
    processed: u16,
    backpressured: bool,
}

struct PageContext<'a> {
    destination: &'a uob_contracts::ExportDestination,
    provider_kind: &'a str,
    budget: &'a RuntimeResourceBudget,
    durability: Durability,
    limit: PageLimit,
}

enum RecordDisposition {
    Committed(ExportSpoolStatus),
    Retry(ExportSpoolStatus),
    Pressured(ExportSpoolStatus),
}

async fn ingest_stream<C, E, D, S, P>(
    source: &S,
    spool: &P,
    destination: &uob_contracts::ExportDestination,
    provider_kind: &str,
    budget: &RuntimeResourceBudget,
    durability: Durability,
    limit: PageLimit,
) -> Result<StreamProgress, ExportIngestionError>
where
    S: OperationalStore<C, E, D, ExportRecord>,
    P: ExportSpool,
{
    let context = PageContext {
        destination,
        provider_kind,
        budget,
        durability,
        limit,
    };
    let mut progress = StreamProgress {
        last_status: None,
        processed: 0,
        backpressured: false,
    };
    let mut stream_state = None;

    for _ in 0..100 {
        let (namespace, status, page) =
            match load_page::<C, E, D, S, P>(source, spool, &context, &mut stream_state).await {
                Ok(value) => value,
                Err(ExportIngestionError::Source(error))
                    if matches!(
                        error.code(),
                        crate::StorageErrorCode::CapacityExhausted | crate::StorageErrorCode::Busy
                    ) =>
                {
                    progress.last_status = stream_state.take().map(|(_, status)| status);
                    progress.backpressured = true;
                    break;
                }
                Err(error) => return Err(error),
            };

        let has_more = page.has_more
            || page
                .items
                .first()
                .is_some_and(|item| item.cursor != page.resume_cursor);
        let (request, descriptor) = prepare_progress(namespace.clone(), durability, &status, page)?;
        if descriptor.is_none() && request.gaps.is_empty() {
            progress.last_status = Some(status);
            break;
        }

        let outcome = if let Some(descriptor) = descriptor {
            advance_record::<C, E, D, S, P>(
                source,
                spool,
                &context,
                request,
                descriptor,
                status,
                &mut progress.processed,
            )
            .await?
        } else {
            match spool.commit_gaps(request).await {
                Ok(committed) => RecordDisposition::Committed(committed),
                Err(error) if is_pressure(error.code()) => RecordDisposition::Pressured(status),
                Err(error) => return Err(error.into()),
            }
        };
        let status = match outcome {
            RecordDisposition::Committed(status) => status,
            RecordDisposition::Retry(status) => {
                stream_state = Some((namespace, status));
                continue;
            }
            RecordDisposition::Pressured(status) => {
                progress.backpressured = true;
                progress.last_status = Some(status);
                break;
            }
        };
        progress.processed += 1;
        stream_state = Some((namespace, status));
        if !has_more {
            break;
        }
    }
    if let Some((_, status)) = stream_state {
        progress.last_status = Some(status);
    }
    Ok(progress)
}

async fn advance_record<C, E, D, S, P>(
    source: &S,
    spool: &P,
    context: &PageContext<'_>,
    request: ExportSpoolGapCommit,
    descriptor: CommittedRecordDescriptor,
    mut status: ExportSpoolStatus,
    processed: &mut u16,
) -> Result<RecordDisposition, ExportIngestionError>
where
    S: OperationalStore<C, E, D, ExportRecord>,
    P: ExportSpool,
{
    // A proven missing prefix advances independently of the next retained record.
    // If that record is too large, it must remain after the new checkpoint.
    if !request.gaps.is_empty() {
        let gap_request = leading_gap_commit(&request, &descriptor)?;
        match spool.commit_gaps(gap_request).await {
            Ok(committed) => {
                status = committed;
                *processed += 1;
            }
            Err(error) if is_pressure(error.code()) => {
                return Ok(RecordDisposition::Pressured(status));
            }
            Err(error) => return Err(error.into()),
        }
    }

    let begin = record_begin(&request, &status, &descriptor);
    let admission = match spool.begin_record(begin).await {
        Ok(admission) => admission,
        Err(error) if is_pressure(error.code()) => {
            return Ok(RecordDisposition::Pressured(status));
        }
        Err(error) => return Err(error.into()),
    };
    match admission {
        ExportSpoolRecordAdmission::TelemetryDropped(committed) => {
            Ok(RecordDisposition::Committed(*committed))
        }
        ExportSpoolRecordAdmission::Transfer(transfer) => {
            transfer_record::<C, E, D, S>(source, context.budget, &descriptor, transfer, status)
                .await
        }
    }
}

async fn transfer_record<C, E, D, S>(
    source: &S,
    budget: &RuntimeResourceBudget,
    descriptor: &CommittedRecordDescriptor,
    mut transfer: Box<dyn ExportSpoolTransfer>,
    status: ExportSpoolStatus,
) -> Result<RecordDisposition, ExportIngestionError>
where
    S: OperationalStore<C, E, D, ExportRecord>,
{
    match stream_record::<C, E, D, S>(source, budget, descriptor, &mut transfer).await {
        Ok(()) => match transfer.finish().await {
            Ok(committed) => Ok(RecordDisposition::Committed(committed)),
            Err(error) if is_pressure(error.code()) => Ok(RecordDisposition::Pressured(status)),
            Err(error) => Err(error.into()),
        },
        Err(StreamRecordError::Expired) => {
            abort_transfer(transfer).await?;
            Ok(RecordDisposition::Retry(status))
        }
        Err(StreamRecordError::Pressure) => {
            abort_transfer(transfer).await?;
            Ok(RecordDisposition::Pressured(status))
        }
        Err(StreamRecordError::Source(error)) => {
            abort_transfer(transfer).await?;
            Err(error.into())
        }
        Err(StreamRecordError::Spool(error)) => {
            abort_transfer(transfer).await?;
            if is_pressure(error.code()) {
                return Ok(RecordDisposition::Pressured(status));
            }
            Err(error.into())
        }
    }
}

async fn abort_transfer(
    transfer: Box<dyn ExportSpoolTransfer>,
) -> Result<(), ExportIngestionError> {
    match transfer.abort().await {
        Ok(()) => Ok(()),
        // The cancelled lease flags rollback even if its abort request meets a full queue.
        Err(error) if error.code() == ExportSpoolErrorCode::Busy => Ok(()),
        Err(error) => Err(error.into()),
    }
}

enum StreamRecordError {
    Expired,
    Pressure,
    Source(StorageError),
    Spool(ExportSpoolError),
}

async fn stream_record<C, E, D, S>(
    source: &S,
    budget: &RuntimeResourceBudget,
    descriptor: &CommittedRecordDescriptor,
    transfer: &mut Box<dyn ExportSpoolTransfer>,
) -> Result<(), StreamRecordError>
where
    S: OperationalStore<C, E, D, ExportRecord>,
{
    for (field, length) in [
        (CommittedRecordField::RecordId, descriptor.record_id_len),
        (
            CommittedRecordField::CommittedAt,
            descriptor.committed_at_len,
        ),
        (CommittedRecordField::Payload, descriptor.payload_len),
    ] {
        let mut offset = 0;
        while offset < length {
            let chunk = match source
                .read_committed_record_chunk(
                    CommittedRecordChunkQuery {
                        token: descriptor.token.clone(),
                        field,
                        offset,
                        max_bytes: EXPORT_RECORD_CHUNK_BYTES,
                    },
                    budget,
                )
                .await
            {
                Ok(CommittedRecordChunkResult::Data(chunk)) => chunk,
                Ok(CommittedRecordChunkResult::Expired) => return Err(StreamRecordError::Expired),
                Err(error)
                    if matches!(
                        error.code(),
                        crate::StorageErrorCode::CapacityExhausted | crate::StorageErrorCode::Busy
                    ) =>
                {
                    return Err(StreamRecordError::Pressure);
                }
                Err(error) => return Err(StreamRecordError::Source(error)),
            };
            if chunk.next_offset <= offset || chunk.next_offset > length {
                return Err(StreamRecordError::Source(StorageError::new(
                    crate::StorageErrorCode::IntegrityFailure,
                    "source returned an invalid record chunk",
                )));
            }
            offset = chunk.next_offset;
            transfer
                .append(chunk)
                .await
                .map_err(StreamRecordError::Spool)?;
        }
    }
    Ok(())
}

fn is_pressure(code: ExportSpoolErrorCode) -> bool {
    matches!(
        code,
        ExportSpoolErrorCode::Backpressure
            | ExportSpoolErrorCode::Busy
            | ExportSpoolErrorCode::SummaryFull
    )
}

async fn load_page<C, E, D, S, P>(
    source: &S,
    spool: &P,
    context: &PageContext<'_>,
    stream_state: &mut Option<(ExportSpoolNamespace, ExportSpoolStatus)>,
) -> Result<(ExportSpoolNamespace, ExportSpoolStatus, CommittedRecordPage), ExportIngestionError>
where
    S: OperationalStore<C, E, D, ExportRecord>,
    P: ExportSpool,
{
    let durability = context.durability;
    let (namespace, mut status, page) = if let Some((_, status)) = stream_state.as_ref() {
        let after = stream_checkpoint(status, durability).map(|point| point.cursor.clone());
        let page = read_source_page::<C, E, D, S>(
            source,
            after,
            durability,
            context.limit,
            context.budget,
        )
        .await?;
        let (namespace, status) = stream_state.take().expect("source state still present");
        (namespace, status, page)
    } else {
        let first =
            read_source_page::<C, E, D, S>(source, None, durability, context.limit, context.budget)
                .await?;
        let namespace = ExportSpoolNamespace {
            destination: context.destination.clone(),
            provider_kind: context.provider_kind.to_owned(),
            source_generation: first.source_generation.clone(),
        };
        let status = spool.status(namespace.clone()).await?;
        let page = if let Some(checkpoint) = stream_checkpoint(&status, durability) {
            read_source_page::<C, E, D, S>(
                source,
                Some(checkpoint.cursor.clone()),
                durability,
                context.limit,
                context.budget,
            )
            .await?
        } else {
            first
        };
        (namespace, status, page)
    };

    if page.source_generation != namespace.source_generation {
        return Err(ExportIngestionError::InvalidSourceCursor);
    }
    if page.high_water > status_high_water(&status, durability)
        || (page.legacy_baseline_incomplete && !status.legacy_baseline_incomplete)
    {
        status = spool
            .observe(
                namespace.clone(),
                durability,
                page.high_water,
                page.legacy_baseline_incomplete,
            )
            .await?;
    }
    Ok((namespace, status, page))
}

async fn read_source_page<C, E, D, S>(
    source: &S,
    after: Option<CommittedRecordCursor>,
    durability: Durability,
    limit: PageLimit,
    budget: &RuntimeResourceBudget,
) -> Result<CommittedRecordPage, StorageError>
where
    S: OperationalStore<C, E, D, ExportRecord>,
{
    source
        .read_committed_records(
            CommittedRecordQuery {
                after,
                limit,
                durability,
            },
            budget,
        )
        .await
}
