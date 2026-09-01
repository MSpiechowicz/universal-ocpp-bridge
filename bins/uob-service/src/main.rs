use std::{error::Error, net::SocketAddr};

use uob_contracts::{ArtifactDigest, BridgeId, ReleaseId};
use uob_service::{StartupIdentityConfiguration, compose};
use uob_target_adapter::TargetRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let identity = StartupIdentityConfiguration::production(
        BridgeId::new("default")?,
        ReleaseId::new(env!("CARGO_PKG_VERSION"))?,
        ArtifactDigest::new(option_env!("UOB_RELEASE_DIGEST").unwrap_or("sha256:development"))?,
    );
    let mut targets = TargetRegistry::<(), ()>::new();
    targets.declare_first_release_unavailable_targets()?;
    let service = compose(targets, identity, None)?;
    let address = SocketAddr::from(([127, 0, 0, 1], 8080));
    uob_management_adapter::serve(address, service.application).await?;
    Ok(())
}
