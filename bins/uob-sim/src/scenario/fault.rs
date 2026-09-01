use std::sync::Arc;
use std::time::Duration;

use super::{FailureCategory, RunFailure, ScenarioClock};
use crate::ProtocolClient;

pub(super) async fn complete_heartbeat_pair_out_of_order(
    client: &dyn ProtocolClient,
    clock: &Arc<dyn ScenarioClock>,
    delay: Duration,
) -> Result<String, RunFailure> {
    let first = async {
        let result = client.heartbeat().await;
        clock.sleep(delay).await;
        result
    };
    let second = client.heartbeat();
    let (first, second) = tokio::join!(first, second);
    second.map_err(|_| heartbeat_failure())?;
    first.map_err(|_| heartbeat_failure())
}

fn heartbeat_failure() -> RunFailure {
    RunFailure::new(
        FailureCategory::Assertion,
        "heartbeat_failed",
        "Heartbeat exchange failed",
    )
}
