use std::{
    marker::PhantomData,
    path::Path,
    sync::{
        Arc,
        mpsc::{self, TrySendError},
    },
};

use rusqlite::Connection;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;
use uob_application::{
    AtomicStoreWrite, AtomicWriteOutcome, CommittedRecord, CommittedRecordCursor,
    CommittedRecordQuery, DeliveryAttempt, DeliveryId, OperationalStore, Page,
    PendingDeliveryQuery, RETAINED_EVENT_CURSOR_PREFIX, RecordedDeliveryAttempt, RecoveryBatch,
    RecoveryQuery, RetainedEventCursor, RetainedEventPage, RetainedEventQuery, ScheduledDelivery,
    SnapshotCursor, SnapshotQuery, StorageError, StorageErrorCode, StorageFuture,
    StorageRetentionStatus, TargetDeliveryStore,
};
use uob_contracts::{Command, RequestId, StationSnapshot, UtcTimestamp};

use crate::{
    SqliteRetentionPolicy, SqliteRuntimeConfiguration, codec,
    configuration::{configure, unavailable},
    worker::{self, Reply, Request},
};

/// Default maximum number of operations waiting for the dedicated `SQLite` worker.
pub const DEFAULT_WORK_QUEUE_CAPACITY: usize = 64;

/// One bounded, single-worker `SQLite` implementation of [`OperationalStore`].
type StoreTypes<C, E, D, R> = fn() -> (C, E, D, R);

pub struct SqliteOperationalStore<C, E, D, R> {
    sender: Arc<crate::lifecycle::Worker<Request<C, E, D, R>>>,
    configuration: SqliteRuntimeConfiguration,
    retention_policy: SqliteRetentionPolicy,
    marker: PhantomData<StoreTypes<C, E, D, R>>,
}

impl<C, E, D, R> Clone for SqliteOperationalStore<C, E, D, R> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            configuration: self.configuration.clone(),
            retention_policy: self.retention_policy,
            marker: PhantomData,
        }
    }
}

impl<C, E, D, R> SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + 'static,
    E: DeserializeOwned + Send + 'static,
    D: DeserializeOwned + Send + 'static,
    R: DeserializeOwned + Send + 'static,
{
    /// Opens or creates a file-backed operational database.
    ///
    /// # Errors
    ///
    /// Returns a sanitized storage error if the queue bound is zero, the database cannot be
    /// opened, required PRAGMAs do not take effect, or the bundled `SQLite` is too old.
    pub fn open(path: impl AsRef<Path>, queue_capacity: usize) -> Result<Self, StorageError> {
        let connection = Connection::open(path).map_err(unavailable)?;
        Self::from_connection(connection, queue_capacity, SqliteRetentionPolicy::default())
    }

    /// Opens a store with explicit logical journal/outbox and completion-reserve limits.
    ///
    /// # Errors
    ///
    /// Returns a sanitized storage error for invalid limits or when the database cannot be
    /// opened, configured, migrated, or assigned a worker thread.
    pub fn open_with_retention_policy(
        path: impl AsRef<Path>,
        queue_capacity: usize,
        retention_policy: SqliteRetentionPolicy,
    ) -> Result<Self, StorageError> {
        let connection = Connection::open(path).map_err(unavailable)?;
        Self::from_connection(connection, queue_capacity, retention_policy)
    }

    /// Returns settings verified before the storage worker accepted any operation.
    #[must_use]
    pub const fn runtime_configuration(&self) -> &SqliteRuntimeConfiguration {
        &self.configuration
    }

    fn from_connection(
        connection: Connection,
        queue_capacity: usize,
        retention_policy: SqliteRetentionPolicy,
    ) -> Result<Self, StorageError> {
        if queue_capacity == 0 {
            return Err(StorageError::new(
                StorageErrorCode::InvalidRequest,
                "storage worker queue capacity must be greater than zero",
            ));
        }
        let retention_policy = SqliteRetentionPolicy::new(
            retention_policy.budget_bytes,
            retention_policy.active_session_reserve_bytes,
        )?;
        let configuration = configure(&connection)?;
        let (sender, receiver) = mpsc::sync_channel(queue_capacity);
        let join = std::thread::Builder::new()
            .name("uob-sqlite-operational-store".to_owned())
            .spawn(move || worker::run(connection, receiver, retention_policy))
            .map_err(|_| {
                StorageError::new(
                    StorageErrorCode::Unavailable,
                    "operational SQLite worker could not start",
                )
            })?;
        Ok(Self {
            sender: Arc::new(crate::lifecycle::Worker::new(sender, join)),
            configuration,
            retention_policy,
            marker: PhantomData,
        })
    }

    /// Stops admission across all clones, drains accepted operations, and joins the worker.
    ///
    /// Call after stopping producers. A timeout closes admission but retains worker ownership:
    /// call again to finish joining. Never delete WAL files or restore older data on timeout.
    /// SQLite commits/rollbacks remain authoritative even if the process must be terminated.
    ///
    /// # Errors
    /// Reports worker failure or a missed deadline without claiming a completed drain.
    pub async fn shutdown(&self, deadline: std::time::Duration) -> Result<(), StorageError> {
        self.sender.shutdown(deadline).await
    }

    fn request<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Request<C, E, D, R>,
    ) -> StorageFuture<'static, T>
    where
        T: Send + 'static,
    {
        let (reply, receiver) = oneshot::channel();
        match self.sender.try_send(build(reply)) {
            Ok(()) => Box::pin(async move {
                receiver.await.map_err(|_| {
                    StorageError::new(
                        StorageErrorCode::Unavailable,
                        "operational SQLite worker stopped",
                    )
                })?
            }),
            Err(TrySendError::Full(_)) => ready_error(
                StorageErrorCode::Busy,
                "operational SQLite worker queue is full",
            ),
            Err(TrySendError::Disconnected(_)) => ready_error(
                StorageErrorCode::Unavailable,
                "operational SQLite worker stopped",
            ),
        }
    }
}

impl<C, E, D, R> OperationalStore<C, E, D, R> for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: Serialize + DeserializeOwned + Send + Sync + 'static,
    D: Serialize + DeserializeOwned + Send + Sync + 'static,
    R: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn write_atomic(
        &self,
        write: AtomicStoreWrite<C, E, D, R>,
    ) -> StorageFuture<'_, AtomicWriteOutcome> {
        let encoded = match codec::encode_write(write) {
            Ok(encoded) => encoded,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.request(|reply| Request::Write(encoded, reply))
    }

    fn read_snapshots(
        &self,
        query: SnapshotQuery,
    ) -> StorageFuture<'_, Page<StationSnapshot, SnapshotCursor>> {
        let after = query.after.map(|cursor| cursor.as_str().to_owned());
        self.request(|reply| Request::Snapshots(after, usize::from(query.limit.get()), reply))
    }

    fn read_retained_events(
        &self,
        query: RetainedEventQuery,
    ) -> StorageFuture<'_, RetainedEventPage<E>> {
        let resource = match codec::resource_key(&query.resource) {
            Ok(resource) => resource,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let after = match event_cursor(query.after.as_ref()) {
            Ok(after) => after,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.request(|reply| {
            Request::Events(resource, after, usize::from(query.limit.get()), reply)
        })
    }

    fn read_committed_records(
        &self,
        query: CommittedRecordQuery,
    ) -> StorageFuture<'_, Page<CommittedRecord<R>, CommittedRecordCursor>> {
        let after = match numeric_cursor(query.after.as_ref().map(CommittedRecordCursor::as_str)) {
            Ok(after) => after,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.request(|reply| {
            Request::Records(
                after,
                usize::from(query.limit.get()),
                query.include_best_effort_telemetry,
                reply,
            )
        })
    }

    fn recover(&self, query: RecoveryQuery) -> StorageFuture<'_, RecoveryBatch<C, D>> {
        self.request(|reply| Request::Recover(usize::from(query.limit.get()), reply))
    }

    fn command_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<Command<C>>> {
        self.request(|reply| Request::Command(request_id.as_str().to_owned(), reply))
    }

    fn command_result_by_request_id(
        &self,
        request_id: RequestId,
    ) -> StorageFuture<'_, Option<uob_contracts::CommandResult>> {
        self.request(|reply| Request::CommandResult(request_id.as_str().to_owned(), reply))
    }

    fn prune_command_deduplication(&self, now: UtcTimestamp) -> StorageFuture<'_, u64> {
        self.request(|reply| Request::PruneCommands(now.into_inner().unix_timestamp(), reply))
    }

    fn maintain_storage_retention(
        &self,
        now: UtcTimestamp,
    ) -> StorageFuture<'_, StorageRetentionStatus> {
        self.request(|reply| Request::MaintainRetention(now.into_inner().unix_timestamp(), reply))
    }

    fn storage_retention_status(&self) -> StorageFuture<'_, StorageRetentionStatus> {
        self.request(Request::RetentionStatus)
    }
}

impl<C, E, D, R> TargetDeliveryStore<D> for SqliteOperationalStore<C, E, D, R>
where
    C: Serialize + DeserializeOwned + Send + Sync + 'static,
    E: Serialize + DeserializeOwned + Send + Sync + 'static,
    D: Serialize + DeserializeOwned + Send + Sync + 'static,
    R: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn read_pending_deliveries(
        &self,
        query: PendingDeliveryQuery,
    ) -> StorageFuture<'_, Vec<ScheduledDelivery<D>>> {
        let Ok(revision) = i64::try_from(query.target_configuration_revision) else {
            return ready_error(
                StorageErrorCode::InvalidRequest,
                "target revision exceeds SQLite integer range",
            );
        };
        let ready_at = match codec::timestamp_key(&query.ready_at) {
            Ok(ready_at) => ready_at,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.request(|reply| {
            Request::PendingDeliveries(
                query.target_instance_id.as_str().to_owned(),
                revision,
                ready_at,
                usize::from(query.limit.get()),
                reply,
            )
        })
    }

    fn record_delivery_attempt(&self, attempt: DeliveryAttempt) -> StorageFuture<'_, ()> {
        let attempt = match codec::encode_delivery_attempt(&attempt) {
            Ok(attempt) => attempt,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.request(|reply| Request::RecordDeliveryAttempt(attempt, reply))
    }

    fn delivery_attempts(
        &self,
        delivery_id: DeliveryId,
        limit: uob_application::PageLimit,
    ) -> StorageFuture<'_, Vec<RecordedDeliveryAttempt>> {
        self.request(|reply| {
            Request::DeliveryAttempts(
                delivery_id.as_str().to_owned(),
                usize::from(limit.get()),
                reply,
            )
        })
    }
}

fn event_cursor(value: Option<&RetainedEventCursor>) -> Result<Option<i64>, StorageError> {
    value
        .map(|value| {
            value
                .as_str()
                .strip_prefix(RETAINED_EVENT_CURSOR_PREFIX)
                .and_then(|position| position.parse::<i64>().ok())
                .filter(|position| *position > 0)
                .ok_or_else(|| {
                    StorageError::new(
                        StorageErrorCode::CursorExpired,
                        "durable event cursor expired; fetch a fresh snapshot",
                    )
                })
        })
        .transpose()
}

fn numeric_cursor(value: Option<&str>) -> Result<Option<i64>, StorageError> {
    value
        .map(|value| {
            value.parse::<i64>().map_err(|_| {
                StorageError::new(
                    StorageErrorCode::CursorExpired,
                    "storage cursor is outside retained state",
                )
            })
        })
        .transpose()
}

fn ready_error<T: Send + 'static>(
    code: StorageErrorCode,
    detail: &'static str,
) -> StorageFuture<'static, T> {
    Box::pin(async move { Err(StorageError::new(code, detail)) })
}
