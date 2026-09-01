use std::sync::atomic::{AtomicUsize, Ordering};

use uob_application::{
    CanonicalQuerySource, RetainedEventQuery, TargetPortError, TargetPortErrorCode,
    TargetPortFuture, TargetQuery, TargetQueryAuthorization, TargetQueryResult,
    TargetRetainedEventStream,
};

#[derive(Default)]
pub struct CountingQuerySource {
    calls: AtomicUsize,
}

impl CountingQuerySource {
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl CanonicalQuerySource<()> for CountingQuerySource {
    fn query<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        _query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<()>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "source",
            ))
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        _query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<()>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "source",
            ))
        })
    }
}
