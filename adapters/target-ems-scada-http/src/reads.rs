use std::{sync::Arc, time::Duration};

use uob_application::{
    Page, SnapshotCursor, TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery,
    TargetQueryPort, TargetQueryResult,
};
use uob_contracts::{DataPointDescriptor, DataPointValue, StationSnapshot};

use crate::error::IntegrationErrorCode;

/// Exactly the canonical reads this listener serves, with the host event payload type erased.
///
/// Retained events use a separate bounded subscription surface; erasing the
/// host's payload parameter here keeps the router and its handlers free of a type parameter that
/// only the supervised session can name.
pub(crate) enum CanonicalRead {
    /// Latest durable command lifecycle, when known and in scope.
    CommandResult(Box<Option<uob_contracts::CommandResult>>),
    /// One current station snapshot, when known and in scope.
    StationSnapshot(Box<Option<StationSnapshot>>),
    /// Bounded page of current station snapshots.
    StationSnapshots(Page<StationSnapshot, SnapshotCursor>),
    /// One canonical point descriptor, when known and in scope.
    DataPointDescriptor(Box<Option<DataPointDescriptor>>),
    /// One current canonical point value, when known and in scope.
    DataPointValue(Box<Option<DataPointValue>>),
}

/// Canonical read surface the integration handlers are given.
///
/// The listener owns no storage and no business handler: every read below is answered by the
/// host's scoped query port, which applies the configured target instance's own authorization
/// before this adapter applies the calling credential's narrower scope.
pub(crate) trait IntegrationReads: Send + Sync {
    /// Executes one canonical read through the supervised scoped query port.
    fn read(&self, query: TargetQuery) -> TargetPortFuture<'_, CanonicalRead>;
}

/// Erases the host event payload type from one supervised scoped query port.
pub(crate) struct SupervisedReads<E>(Arc<dyn TargetQueryPort<E>>);

impl<E> SupervisedReads<E> {
    pub(crate) const fn new(port: Arc<dyn TargetQueryPort<E>>) -> Self {
        Self(port)
    }
}

impl<E: Send + 'static> IntegrationReads for SupervisedReads<E> {
    fn read(&self, query: TargetQuery) -> TargetPortFuture<'_, CanonicalRead> {
        Box::pin(async move {
            match self.0.query(query).await? {
                TargetQueryResult::StationSnapshot(snapshot) => {
                    Ok(CanonicalRead::StationSnapshot(Box::new(snapshot)))
                }
                TargetQueryResult::StationSnapshots(page) => {
                    Ok(CanonicalRead::StationSnapshots(page))
                }
                TargetQueryResult::DataPointDescriptor(descriptor) => {
                    Ok(CanonicalRead::DataPointDescriptor(Box::new(descriptor)))
                }
                TargetQueryResult::DataPointValue(value) => {
                    Ok(CanonicalRead::DataPointValue(Box::new(value)))
                }
                TargetQueryResult::CommandResult(result) => {
                    Ok(CanonicalRead::CommandResult(Box::new(result)))
                }
                TargetQueryResult::Capabilities(_) | TargetQueryResult::RetainedEvents(_) => {
                    Err(TargetPortError::new(
                        TargetPortErrorCode::InvalidRequest,
                        "query.response_type_mismatch",
                    ))
                }
            }
        })
    }
}

/// Applies the listener's own bounded deadline around every canonical read.
///
/// A slow authoritative source therefore releases the caller's concurrency permit instead of
/// holding one of the listener's bounded client slots open indefinitely.
#[derive(Clone)]
pub(crate) struct ReadExecutor {
    reads: Arc<dyn IntegrationReads>,
    deadline: Duration,
}

impl ReadExecutor {
    pub(crate) const fn new(reads: Arc<dyn IntegrationReads>, deadline: Duration) -> Self {
        Self { reads, deadline }
    }

    /// Executes one canonical read, mapping every failure onto a stable integration code.
    pub(crate) async fn read(
        &self,
        query: TargetQuery,
    ) -> Result<CanonicalRead, IntegrationErrorCode> {
        match tokio::time::timeout(self.deadline, self.reads.read(query)).await {
            Err(_) => Err(IntegrationErrorCode::DeadlineExceeded),
            Ok(Ok(read)) => Ok(read),
            Ok(Err(error)) => Err(integration_code(error.code())),
        }
    }

    /// Reads one station snapshot page.
    pub(crate) async fn station_snapshots(
        &self,
        query: uob_application::SnapshotQuery,
    ) -> Result<Page<StationSnapshot, SnapshotCursor>, IntegrationErrorCode> {
        match self.read(TargetQuery::StationSnapshots(query)).await? {
            CanonicalRead::StationSnapshots(page) => Ok(page),
            _ => Err(IntegrationErrorCode::SourceUnavailable),
        }
    }

    /// Reads one current station snapshot.
    pub(crate) async fn station_snapshot(
        &self,
        resource: uob_contracts::ResourceRef,
    ) -> Result<Option<StationSnapshot>, IntegrationErrorCode> {
        match self.read(TargetQuery::StationSnapshot(resource)).await? {
            CanonicalRead::StationSnapshot(snapshot) => Ok(*snapshot),
            _ => Err(IntegrationErrorCode::SourceUnavailable),
        }
    }
}

/// Translates a scoped-port failure into this listener's stable error vocabulary.
///
/// The port's own context string is deliberately dropped: integration clients branch on the
/// documented `ems_scada_http.*` codes, and no host-internal reason reaches the wire.
pub(crate) const fn integration_code(code: TargetPortErrorCode) -> IntegrationErrorCode {
    match code {
        TargetPortErrorCode::Unauthorized => IntegrationErrorCode::PermissionDenied,
        TargetPortErrorCode::Unsupported => IntegrationErrorCode::OperationNotSupported,
        TargetPortErrorCode::Expired => IntegrationErrorCode::Expired,
        TargetPortErrorCode::Busy => IntegrationErrorCode::CapacityExhausted,
        TargetPortErrorCode::Unavailable => IntegrationErrorCode::SourceUnavailable,
        TargetPortErrorCode::InvalidRequest => IntegrationErrorCode::InvalidRequest,
        TargetPortErrorCode::CursorExpired => IntegrationErrorCode::CursorExpired,
    }
}
