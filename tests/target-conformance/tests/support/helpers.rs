use std::future::Future;

use time::OffsetDateTime;
use tokio::time::Duration;
use uob_contracts::UtcTimestamp;

pub(crate) fn timestamp(minute: i64) -> UtcTimestamp {
    UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(minute))
}

pub(crate) async fn timeout<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(1), future)
        .await
        .expect("scenario timed out")
}

pub(crate) fn text<T, E: std::fmt::Debug>(
    constructor: impl FnOnce(String) -> Result<T, E>,
    value: impl Into<String>,
) -> T {
    constructor(value.into()).expect("valid fixture text")
}
