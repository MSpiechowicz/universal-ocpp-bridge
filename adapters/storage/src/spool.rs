use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
};

use tokio::sync::oneshot;
use uob_application::{
    BudgetedRecordChunk, CommittedRecordField, Durability, ExportPendingDescriptor,
    ExportPendingPage, ExportPendingPosition, ExportSpool, ExportSpoolError, ExportSpoolErrorCode,
    ExportSpoolFuture, ExportSpoolGapCommit, ExportSpoolNamespace, ExportSpoolRecordAdmission,
    ExportSpoolRecordBegin, ExportSpoolStatus, ExportSpoolTransfer, PageLimit,
    RuntimeResourceBudget,
};

mod filesystem;
mod pressure;
mod schema;
mod store;
mod store_commit;
mod worker;

/// Main file at most 60 MiB; a full DELETE rollback journal stays under 62 MiB.
const MAIN_FILE_BYTES: u64 = 60 * 1024 * 1024;
const MAX_GAPS: usize = 512;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub main_bytes: u64,
    pub gap_slots: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            main_bytes: MAIN_FILE_BYTES,
            gap_slots: MAX_GAPS,
        }
    }
}

impl Limits {
    /// Only smaller test quotas are accepted; two slots remain reserved for source loss.
    /// # Errors
    /// Returns `InvalidRequest` for an unsupported quota.
    pub fn new(main_bytes: u64, gap_slots: usize) -> Result<Self, ExportSpoolError> {
        if !(1024 * 1024..=MAIN_FILE_BYTES).contains(&main_bytes)
            || !(2..=MAX_GAPS).contains(&gap_slots)
        {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::InvalidRequest,
                "invalid export spool limit",
            ));
        }
        Ok(Self {
            main_bytes: main_bytes / 4096 * 4096,
            gap_slots,
        })
    }
}

type Reply<T> = oneshot::Sender<Result<T, ExportSpoolError>>;
enum Request {
    Status(ExportSpoolNamespace, Reply<ExportSpoolStatus>),
    Observe(
        ExportSpoolNamespace,
        Durability,
        u64,
        bool,
        Reply<ExportSpoolStatus>,
    ),
    CommitGaps(ExportSpoolGapCommit, Reply<ExportSpoolStatus>),
    Begin(ExportSpoolRecordBegin, Reply<ExportSpoolRecordAdmission>),
    Append(u64, BudgetedRecordChunk, Reply<()>),
    Finish(u64, Reply<ExportSpoolStatus>),
    Abort(u64, Reply<()>),
    Wake,
    Pending(
        ExportSpoolNamespace,
        Option<ExportPendingPosition>,
        PageLimit,
        Reply<ExportPendingPage>,
    ),
    PendingChunk(
        ExportSpoolNamespace,
        ExportPendingDescriptor,
        CommittedRecordField,
        u64,
        usize,
        Reply<BudgetedRecordChunk>,
    ),
}

fn busy() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::Busy,
        "export spool transfer in progress",
    )
}

fn unavailable() -> ExportSpoolError {
    ExportSpoolError::new(
        ExportSpoolErrorCode::Unavailable,
        "export spool worker stopped",
    )
}

struct Transfer {
    id: u64,
    sender: Arc<SyncSender<Request>>,
    cancelled: Arc<AtomicBool>,
    finished: bool,
}

impl Drop for Transfer {
    fn drop(&mut self) {
        if !self.finished {
            self.cancelled.store(true, Ordering::Release);
            let _ = self.sender.try_send(Request::Wake);
        }
    }
}

impl Transfer {
    fn request<T: Send + 'static>(
        &self,
        build: impl FnOnce(Reply<T>) -> Request,
    ) -> ExportSpoolFuture<'static, T> {
        send(&self.sender, build)
    }
}

impl ExportSpoolTransfer for Transfer {
    fn append(&mut self, chunk: BudgetedRecordChunk) -> ExportSpoolFuture<'_, ()> {
        let id = self.id;
        self.request(|reply| Request::Append(id, chunk, reply))
    }

    fn finish(self: Box<Self>) -> ExportSpoolFuture<'static, ExportSpoolStatus> {
        Box::pin(async move {
            let mut transfer = self;
            let id = transfer.id;
            let result = transfer.request(|reply| Request::Finish(id, reply)).await;
            if result.is_ok() {
                transfer.finished = true;
            }
            result
        })
    }

    fn abort(self: Box<Self>) -> ExportSpoolFuture<'static, ()> {
        Box::pin(async move {
            let mut transfer = self;
            transfer.cancelled.store(true, Ordering::Release);
            let id = transfer.id;
            let result = transfer.request(|reply| Request::Abort(id, reply)).await;
            if result.is_ok() {
                transfer.finished = true;
            }
            result
        })
    }
}

fn send<T: Send + 'static>(
    sender: &SyncSender<Request>,
    build: impl FnOnce(Reply<T>) -> Request,
) -> ExportSpoolFuture<'static, T> {
    let (reply, result) = oneshot::channel();
    match sender.try_send(build(reply)) {
        Ok(()) => Box::pin(async move { result.await.map_err(|_| unavailable())? }),
        Err(TrySendError::Full(_)) => Box::pin(async { Err(busy()) }),
        Err(TrySendError::Disconnected(_)) => Box::pin(async { Err(unavailable()) }),
    }
}

/// Dedicated bounded worker independent of operational SQLite.
#[derive(Clone)]
pub struct SqliteExportSpool {
    sender: Arc<SyncSender<Request>>,
}

impl SqliteExportSpool {
    /// The private directory must be on a different, independently provisioned device.
    /// # Errors
    /// Rejects invalid isolation, quota, schema, or worker startup.
    pub fn open(
        directory: impl AsRef<Path>,
        operational_database: impl AsRef<Path>,
        queue_capacity: usize,
        budget: &RuntimeResourceBudget,
    ) -> Result<Self, ExportSpoolError> {
        Self::open_with_limits(
            directory,
            operational_database,
            queue_capacity,
            Limits::default(),
            budget,
        )
    }

    /// Opens with a smaller physical and gap-summary quota.
    /// # Errors
    /// Rejects invalid isolation, quota, schema, or worker startup.
    pub fn open_with_limits(
        directory: impl AsRef<Path>,
        operational_database: impl AsRef<Path>,
        queue_capacity: usize,
        limits: Limits,
        budget: &RuntimeResourceBudget,
    ) -> Result<Self, ExportSpoolError> {
        if queue_capacity == 0
            || limits.main_bytes > MAIN_FILE_BYTES
            || limits.gap_slots > MAX_GAPS
            || limits.gap_slots < 2
            || limits.main_bytes < 1024 * 1024
        {
            return Err(ExportSpoolError::new(
                ExportSpoolErrorCode::InvalidRequest,
                "invalid spool limits or worker queue capacity",
            ));
        }
        let budget = budget.clone();
        let connection = filesystem::open_database(
            directory.as_ref(),
            operational_database.as_ref(),
            limits,
            &budget,
        )?;
        let (sender, receiver) = mpsc::sync_channel(queue_capacity);
        let sender = Arc::new(sender);
        worker::spawn(
            connection,
            Arc::downgrade(&sender),
            receiver,
            limits,
            budget,
        )?;
        Ok(Self { sender })
    }

    fn request<T: Send + 'static>(
        &self,
        build: impl FnOnce(Reply<T>) -> Request,
    ) -> ExportSpoolFuture<'static, T> {
        send(&self.sender, build)
    }
}

impl ExportSpool for SqliteExportSpool {
    fn status(&self, namespace: ExportSpoolNamespace) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        self.request(|reply| Request::Status(namespace, reply))
    }
    fn observe(
        &self,
        namespace: ExportSpoolNamespace,
        durability: Durability,
        high_water: u64,
        legacy_baseline_incomplete: bool,
    ) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        self.request(|reply| {
            Request::Observe(
                namespace,
                durability,
                high_water,
                legacy_baseline_incomplete,
                reply,
            )
        })
    }
    fn commit_gaps(
        &self,
        request: ExportSpoolGapCommit,
    ) -> ExportSpoolFuture<'_, ExportSpoolStatus> {
        self.request(|reply| Request::CommitGaps(request, reply))
    }
    fn begin_record(
        &self,
        request: ExportSpoolRecordBegin,
    ) -> ExportSpoolFuture<'_, ExportSpoolRecordAdmission> {
        self.request(|reply| Request::Begin(request, reply))
    }
    fn pending(
        &self,
        namespace: ExportSpoolNamespace,
        after: Option<ExportPendingPosition>,
        limit: PageLimit,
    ) -> ExportSpoolFuture<'_, ExportPendingPage> {
        self.request(|reply| Request::Pending(namespace, after, limit, reply))
    }
    fn pending_chunk(
        &self,
        namespace: ExportSpoolNamespace,
        descriptor: ExportPendingDescriptor,
        field: CommittedRecordField,
        offset: u64,
        max_bytes: usize,
    ) -> ExportSpoolFuture<'_, BudgetedRecordChunk> {
        self.request(|reply| {
            Request::PendingChunk(namespace, descriptor, field, offset, max_bytes, reply)
        })
    }
}

fn fail(error: &rusqlite::Error) -> ExportSpoolError {
    let code = match error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DiskFull =>
        {
            ExportSpoolErrorCode::Backpressure
        }
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseBusy =>
        {
            ExportSpoolErrorCode::Busy
        }
        _ => ExportSpoolErrorCode::Unavailable,
    };
    ExportSpoolError::new(code, "export spool SQLite operation failed")
}
