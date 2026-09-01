use std::{error::Error, net::SocketAddr};

use uob_service::compose;
use uob_target_adapter::TargetRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let service = compose(TargetRegistry::<(), ()>::new());
    let address = SocketAddr::from(([127, 0, 0, 1], 8080));
    uob_management_adapter::serve(address, service.application).await?;
    Ok(())
}
